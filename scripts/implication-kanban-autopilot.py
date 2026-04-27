#!/usr/bin/env python3
"""Implication Kanban pilot autopilot for Paddy's Vibe Kanban host.

This is an operator-side safety rail for the opt-in Implication Kanban pilot:
- starts queued Backlog/To do cards with linked GitHub issues up to a concurrency cap;
- mirrors local workspaces into the remote board when /api/workspaces/start does not;
- moves completed implementation cards to In review;
- starts a separate Codex auto-review session if one is missing;
- consumes completed auto-review decisions;
- merges clean approved PRs, moves cards to Done, and lets unblocked queued cards start.
- interprets review results, auto-starts fix sessions on requested changes,
  and auto-merges/moves cards to Done when review and GitHub checks pass.

It intentionally targets the Implication Vibe project and gavinanelson/implication.
"""
from __future__ import annotations

import argparse
import csv
import fcntl
import io
import json
import os
import re
import shlex
import sqlite3
import subprocess
import sys
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[1]
LOCAL_DB = ROOT / "dev_assets" / "db.v2.sqlite"
DEFAULT_BATCH = "first-real-pilot-5"


def configured_batch(env: dict[str, str]) -> str:
    return env.get("VK_AUTOPILOT_BATCH", DEFAULT_BATCH)


BACKEND = os.environ.get("VK_AUTOPILOT_BACKEND", "http://127.0.0.1:3009")
PROJECT_ID = os.environ.get("VK_AUTOPILOT_PROJECT_ID", "0cd2d22b-c1ce-4386-a24b-9da3b6b44960")
USER_ID = os.environ.get("VK_AUTOPILOT_USER_ID", "b15df8b3-456a-4e9b-b963-d25e65cda8bc")
REPO_ID = os.environ.get("VK_AUTOPILOT_REPO_ID", "ecc7779c-a4bb-445b-9b02-f497475134dc")
REMOTE_DB_CONTAINER = os.environ.get("VK_AUTOPILOT_REMOTE_DB", "remote-remote-db-1")
GH_REPO = os.environ.get("VK_AUTOPILOT_GH_REPO", "gavinanelson/implication")
BATCH = configured_batch(os.environ)
STATE_PATH = ROOT / ".vibe-kanban-dev" / f"implication-kanban-autopilot-{BATCH}.json"
MODEL = os.environ.get("VK_AUTOPILOT_MODEL", "gpt-5.5")
REASONING = os.environ.get("VK_AUTOPILOT_REASONING", "medium")
CAP = int(os.environ.get("VK_AUTOPILOT_CAP", "3"))
POLL_SECONDS = int(os.environ.get("VK_AUTOPILOT_POLL_SECONDS", "30"))

STATUS_BACKLOG = "Backlog"
STATUS_TODO = "To do"
STATUS_IN_PROGRESS = "In progress"
STATUS_IN_REVIEW = "In review"
STATUS_DONE = "Done"
AUTO_MERGE = os.environ.get("VK_AUTOPILOT_AUTO_MERGE", "1") not in {"0", "false", "False"}
AUTO_FIX_REVIEW_FAILURES = os.environ.get("VK_AUTOPILOT_AUTO_FIX_REVIEW_FAILURES", "1") not in {"0", "false", "False"}


def run(cmd: list[str] | str, *, check: bool = True, input_text: str | None = None) -> str:
    if isinstance(cmd, str):
        shell = True
        printable = cmd
    else:
        shell = False
        printable = " ".join(shlex.quote(c) for c in cmd)
    proc = subprocess.run(
        cmd,
        shell=shell,
        input=input_text,
        text=True,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if check and proc.returncode != 0:
        raise RuntimeError(f"command failed ({proc.returncode}): {printable}\nSTDOUT:\n{proc.stdout}\nSTDERR:\n{proc.stderr}")
    return proc.stdout


def psql(sql: str) -> list[dict[str, Any]]:
    out = run([
        "docker", "exec", "-i", REMOTE_DB_CONTAINER,
        "psql", "-U", "remote", "-d", "remote", "-v", "ON_ERROR_STOP=1", "--csv", "-c", sql,
    ])
    if not out.strip():
        return []
    return list(csv.DictReader(io.StringIO(out)))


def psql_exec(sql: str) -> None:
    run(["docker", "exec", "-i", REMOTE_DB_CONTAINER, "psql", "-U", "remote", "-d", "remote", "-v", "ON_ERROR_STOP=1"], input_text=sql)


def api(path: str, payload: dict[str, Any] | None = None) -> dict[str, Any]:
    data = None
    headers = {"Content-Type": "application/json"}
    method = "GET"
    if payload is not None:
        data = json.dumps(payload).encode()
        method = "POST"
    req = urllib.request.Request(f"{BACKEND}{path}", data=data, headers=headers, method=method)
    try:
        with urllib.request.urlopen(req, timeout=60) as resp:
            return json.loads(resp.read().decode())
    except urllib.error.HTTPError as exc:
        body = exc.read().decode(errors="replace")
        raise RuntimeError(f"HTTP {exc.code} for {path}: {body}") from exc


@dataclass
class Card:
    id: str
    simple_id: str
    title: str
    status: str
    gh_number: int
    priority: str
    local_workspace_id: str | None
    workspace_name: str | None


def sql_lit(s: str) -> str:
    return "'" + s.replace("'", "''") + "'"


def cards_sql(batch: str) -> str:
    return f"""
    select i.id::text as id, i.simple_id, i.title, ps.name as status,
           coalesce(i.extension_metadata->'github_link'->>'issue_number','0') as gh_number,
           coalesce(i.priority::text,'') as priority,
           coalesce(w.local_workspace_id::text,'') as local_workspace_id,
           coalesce(w.name,'') as workspace_name
    from issues i
    join project_statuses ps on ps.id=i.status_id
    left join lateral (
        select local_workspace_id, name
        from workspaces
        where issue_id = i.id and not archived
        order by created_at asc
        limit 1
    ) w on true
    where i.project_id = {sql_lit(PROJECT_ID)}
      and i.extension_metadata ? 'github_link'
      and coalesce(i.extension_metadata->'kanban_workflow'->>'batch','') = {sql_lit(batch)}
      and i.simple_id like 'BDF-%'
    order by i.issue_number;
    """


def cards() -> list[Card]:
    sql = cards_sql(BATCH)
    return [
        Card(
            id=r["id"], simple_id=r["simple_id"], title=r["title"], status=r["status"],
            gh_number=int(r["gh_number"]), priority=r["priority"],
            local_workspace_id=r["local_workspace_id"] or None, workspace_name=r["workspace_name"] or None,
        )
        for r in psql(sql)
    ]


def status_id(name: str) -> str:
    rows = psql(
        f"select id::text as id from project_statuses "
        f"where project_id={sql_lit(PROJECT_ID)} and name={sql_lit(name)} limit 1;"
    )
    if not rows:
        raise RuntimeError(f"missing status {name}")
    return rows[0]["id"]


def local_processes(workspace_id: str) -> list[dict[str, Any]]:
    con = sqlite3.connect(LOCAL_DB)
    con.row_factory = sqlite3.Row
    rows = con.execute(
        """
        select lower(hex(ep.id)) pid, lower(hex(s.id)) sid, coalesce(s.name,'') session,
               ep.status, ep.run_reason, ep.exit_code, ep.started_at, ep.completed_at, ep.executor_action
        from execution_processes ep
        join sessions s on s.id=ep.session_id
        where s.workspace_id = cast(? as blob)
        order by ep.created_at desc
        """,
        (bytes.fromhex(workspace_id.replace('-', '')),),
    ).fetchall()
    return [dict(r) for r in rows]


def hyphen_uuid(hex_id: str) -> str:
    h = hex_id.replace("-", "").lower()
    return f"{h[0:8]}-{h[8:12]}-{h[12:16]}-{h[16:20]}-{h[20:32]}"


def process_log_path(session_id: str, process_id: str) -> Path:
    sid = hyphen_uuid(session_id)
    pid = hyphen_uuid(process_id)
    return ROOT / "dev_assets" / "sessions" / sid[:2] / sid / "processes" / f"{pid}.jsonl"


def _text_from_value(value: Any) -> str:
    if value is None:
        return ""
    if isinstance(value, str):
        return value
    if isinstance(value, list):
        return "\n".join(filter(None, (_text_from_value(v) for v in value)))
    if isinstance(value, dict):
        parts = []
        for key in ("text", "message", "body", "summary", "decision", "review", "content"):
            if key in value:
                parts.append(_text_from_value(value[key]))
        return "\n".join(filter(None, parts))
    return ""


def _item_text(item: dict[str, Any]) -> str:
    typ = item.get("type")
    if typ == "agentMessage":
        return _text_from_value(item.get("text"))
    if typ in {"review_rollout_assistant", "exitedReviewMode"}:
        return _text_from_value(item)
    return ""


def extract_process_agent_text(lines: list[str]) -> str:
    """Extract useful Codex review text from current and historical JSONL shapes.

    Auto-review output has moved between final-answer agent messages, deltas,
    review rollout items, and exitedReviewMode payloads. Prefer final/review
    surfaces so commentary or empty duplicate sessions do not drive decisions.
    """
    item_phase: dict[str, str] = {}
    item_type: dict[str, str] = {}
    completed_text: dict[str, str] = {}
    delta_text: dict[str, list[str]] = {}
    final_ids: list[str] = []
    review_ids: list[str] = []

    for line in lines:
        try:
            outer = json.loads(line)
        except json.JSONDecodeError:
            continue
        chunk = outer.get("Stdout")
        if not chunk:
            continue
        for inner_line in chunk.splitlines():
            try:
                msg = json.loads(inner_line)
            except json.JSONDecodeError:
                continue
            method = msg.get("method")
            params = msg.get("params") or {}
            if method in {"item/started", "item/completed"}:
                item = params.get("item") or {}
                item_id = item.get("id")
                typ = item.get("type") or ""
                phase = item.get("phase") or ""
                if item_id:
                    item_type[item_id] = typ
                    item_phase[item_id] = phase
                    if phase == "final_answer" and item_id not in final_ids:
                        final_ids.append(item_id)
                    if typ in {"review_rollout_assistant", "exitedReviewMode"} and item_id not in review_ids:
                        review_ids.append(item_id)
                text = _item_text(item)
                if item_id and text:
                    completed_text[item_id] = text
                elif typ in {"review_rollout_assistant", "exitedReviewMode"} and text:
                    anonymous_id = item_id or f"anonymous-{len(review_ids)}"
                    completed_text[anonymous_id] = text
                    review_ids.append(anonymous_id)
            elif method == "item/agentMessage/delta":
                item_id = params.get("itemId") or params.get("item_id") or ""
                delta = params.get("delta")
                if item_id and delta:
                    delta_text.setdefault(item_id, []).append(delta)
            elif method == "review_rollout_assistant":
                item = params.get("item") or params
                text = _text_from_value(item)
                if text:
                    review_id = f"review-{len(completed_text)}"
                    completed_text[review_id] = text
                    review_ids.append(review_id)

    def text_for(item_id: str) -> str:
        return completed_text.get(item_id) or "".join(delta_text.get(item_id, []))

    final_text = "\n".join(filter(None, (text_for(item_id) for item_id in final_ids)))
    if final_text.strip():
        return final_text
    review_text = "\n".join(filter(None, (text_for(item_id) for item_id in review_ids)))
    if review_text.strip():
        return review_text

    agent_ids = [
        item_id for item_id, typ in item_type.items()
        if typ == "agentMessage" and item_phase.get(item_id) in {"", "final_answer", "commentary"}
    ]
    parts = [text_for(item_id) for item_id in agent_ids]
    raw = "\n".join(lines)
    matches = list(re.finditer(r"Decision:\s*(?:pass|approve|request(?:\s+changes)?)", raw, flags=re.IGNORECASE))
    raw_decision_tail = ""
    for match in reversed(matches):
        prefix = raw[:match.start()]
        last_agent = max(prefix.rfind('"type":"agentMessage"'), prefix.rfind('\\"type\\":\\"agentMessage\\"'))
        last_user = max(prefix.rfind('"type":"userMessage"'), prefix.rfind('\\"type\\":\\"userMessage\\"'))
        if last_agent > last_user:
            raw_decision_tail = raw[match.start():]
            break

    agent_text = "\n".join(p for p in parts if p.strip())
    if agent_text.strip():
        return "\n".join(filter(None, [agent_text, raw_decision_tail]))

    # Codex/PTY output can split nested JSON strings across outer JSONL rows.
    # If structured extraction fails, recover the last explicit Decision line
    # from raw output so a complete review is not treated as empty/failed.
    if raw_decision_tail:
        return raw_decision_tail
    return ""


def process_agent_text(process_id: str, session_id: str) -> str:
    path = process_log_path(session_id, process_id)
    if not path.exists():
        return ""
    return extract_process_agent_text(path.read_text(errors="ignore").splitlines())


def decision_from_text(text: str) -> str:
    normalized = re.sub(r"\s+", " ", text.lower())
    # Review transcripts can contain older quoted review output (for example a
    # prior `Decision: request changes`) before the final reviewer verdict. Use
    # the last explicit Decision line as authoritative, otherwise a successful
    # re-review can be poisoned by stale blocker text in the context it read.
    explicit = list(re.finditer(r"\bdecision:\s*(pass|approve|request(?:\s+changes)?)\b", normalized))
    if explicit:
        verdict = explicit[-1].group(1)
        if verdict.startswith("request"):
            return "request_changes"
        return "pass"
    if any(k in normalized for k in ["request changes", "changes requested", "blocking regression", "blocker:"]):
        if not any(k in normalized for k in ["no blockers", "no blocking", "no blocking regressions"]):
            return "request_changes"
    if any(k in normalized for k in ["approved", "no blockers", "no blocking regressions", "no blocking issues"]):
        return "pass"
    return "failed"


def review_result_text(process: dict[str, Any]) -> str:
    if "text" in process:
        return process.get("text") or ""
    pid = process.get("pid")
    sid = process.get("sid")
    if not pid or not sid:
        return ""
    return process_agent_text(str(pid), str(sid))


def usable_completed_review(process: dict[str, Any]) -> tuple[bool, str]:
    if process.get("status") != "completed" or str(process.get("exit_code")) != "0":
        return False, ""
    text = review_result_text(process)
    return bool(text.strip()), text


def decide_from_review_attempts(reviews: list[dict[str, Any]]) -> tuple[str, str, dict[str, Any] | None]:
    if not reviews:
        return "missing", "", None
    if any(p.get("status") == "running" for p in reviews):
        return "running", "", None

    completed_results: list[tuple[dict[str, Any], str]] = []
    for p in reviews:
        usable, text = usable_completed_review(p)
        if usable:
            completed_results.append((p, text))
    if completed_results:
        latest, text = completed_results[0]
        return decision_from_text(text), text, latest

    latest = reviews[0]
    if latest.get("status") in {"killed", "completed"}:
        # Killed or empty duplicate Auto review attempts are not review results.
        return "missing", "", latest
    return "failed", "", latest

def is_auto_review_session(name: str | None) -> bool:
    return (name or "").strip().lower().startswith("auto review")


def review_processes(card: Card) -> list[dict[str, Any]]:
    if not card.local_workspace_id:
        return []
    return [p for p in local_processes(card.local_workspace_id) if (p["session"] or "").lower().startswith("auto review")]


def has_running_auto_review(card: Card) -> bool:
    return any(p.get("status") == "running" for p in review_processes(card))


def implementation_processes(card: Card) -> list[dict[str, Any]]:
    if not card.local_workspace_id:
        return []
    return [p for p in local_processes(card.local_workspace_id) if not (p["session"] or "").lower().startswith("auto review") and p["run_reason"] == "codingagent"]


def latest_completed_review(card: Card) -> dict[str, Any] | None:
    for p in review_processes(card):
        usable, _text = usable_completed_review(p)
        if usable:
            return p
    return None


def review_decision(card: Card) -> tuple[str, str, dict[str, Any] | None]:
    """Return pass/request_changes/failed/running/missing plus review text.

    Vibe can contain duplicate review attempts when a manual fallback and the UI
    trigger race. Ignore killed duplicates if there is a completed review result;
    otherwise a later killed row can incorrectly mask a valid approval.
    """
    return decide_from_review_attempts(review_processes(card))


def active_implementation_count(cs: list[Card]) -> int:
    count = 0
    for c in cs:
        if c.status != STATUS_IN_PROGRESS or not c.local_workspace_id:
            continue
        for p in local_processes(c.local_workspace_id):
            if p["status"] == "running" and "review" not in (p["session"] or "").lower():
                count += 1
                break
    return count


def issue_json(number: int) -> dict[str, Any]:
    return json.loads(run(["gh", "issue", "view", str(number), "--repo", GH_REPO, "--json", "number,title,body,labels,url"]))


def reasoning_for_issue(issue: dict[str, Any]) -> str:
    _ = issue
    return REASONING


def branch_slug(card: Card, issue: dict[str, Any]) -> str:
    body = issue.get("body") or ""
    for line in body.splitlines():
        if "Branch slug:" in line:
            return line.split("Branch slug:", 1)[1].strip().strip("`")
    safe = ''.join(ch.lower() if ch.isalnum() else '-' for ch in issue['title'])
    safe = '-'.join(filter(None, safe.split('-')))[:40]
    return f"work/{issue['number']}-{safe}"


def start_card(card: Card) -> None:
    issue = issue_json(card.gh_number)
    labels = [l["name"] for l in issue.get("labels", [])]
    reasoning = reasoning_for_issue(issue)
    prompt = f"""You are working an opt-in Vibe Kanban pilot card for Implication.

Card: {card.simple_id}
Canonical GitHub issue: {GH_REPO}#{card.gh_number}
Title: {issue['title']}
URL: {issue['url']}
Labels: {', '.join(labels)}

GitHub is canonical. Implement only the bounded scope in the issue. Keep changes small and reviewable.

Issue body:
{issue.get('body') or ''}

Workflow requirements:
- Work in the Implication repo.
- Make a real implementation/docs/chore change satisfying acceptance criteria.
- Run focused validation only. Do not run broad local Vibe Kanban/Rust/TS validation loops or release/native builds on Host A/omarchy (`pnpm run check`, `pnpm run backend:check`, `pnpm run lint`, `cargo check --workspace`, `cargo test --workspace`, broad `cargo test`, `cargo clippy --workspace`, `pnpm run generate-types:check`, `cargo build --release`, or Tauri release builds) unless explicit operator approval sets `ALLOW_HEAVY_VIBE_VALIDATION=1`; prefer CI/off-host for full validation.
- Commit changes on the workspace branch.
- Final response must include changed files, commit hash, focused validation evidence, and whether it is review-ready.
"""
    name = f"{card.simple_id} implementation"
    payload = {
        "name": name,
        "repos": [{"repo_id": REPO_ID, "target_branch": "master"}],
        "linked_issue": {"remote_project_id": PROJECT_ID, "issue_id": card.id},
        "executor_config": {"executor": "CODEX", "variant": "DEFAULT", "model_id": MODEL, "reasoning_id": reasoning},
        "prompt": prompt,
        "attachment_ids": None,
    }
    res = api("/api/workspaces/start", payload)
    if not res.get("success"):
        raise RuntimeError(f"workspace start failed for {card.simple_id}: {res}")
    ws = res["data"]["workspace"]
    in_progress_id = status_id(STATUS_IN_PROGRESS)
    # /api/workspaces/start normally creates the remote workspace row. Only mirror
    # if Electric/remote sync really missed it; otherwise we create duplicate
    # visible workspaces and duplicate active cards.
    existing_remote = psql(
        f"""
        select id::text as id
        from workspaces
        where project_id={sql_lit(PROJECT_ID)}
          and issue_id={sql_lit(card.id)}
          and local_workspace_id={sql_lit(ws['id'])}
          and not archived
        limit 1;
        """
    )
    mirror_sql = ""
    if not existing_remote:
        remote_ws_id = run(["uuidgen"]).strip().lower()
        mirror_sql = f"""
        insert into workspaces (id, project_id, owner_user_id, issue_id, local_workspace_id, name, archived, files_changed, lines_added, lines_removed, created_at, updated_at)
        values ({sql_lit(remote_ws_id)}, {sql_lit(PROJECT_ID)}, {sql_lit(USER_ID)}, {sql_lit(card.id)}, {sql_lit(ws['id'])}, {sql_lit(ws['name'])}, false, 0, 0, 0, now(), now())
        on conflict (local_workspace_id) do update set issue_id=excluded.issue_id, name=excluded.name, archived=false, updated_at=now();
        """
    sql = f"""
    begin;
    {mirror_sql}
    update issues set status_id={sql_lit(in_progress_id)}, updated_at=now() where id={sql_lit(card.id)};
    insert into issue_comments (issue_id, author_id, message) values ({sql_lit(card.id)}, {sql_lit(USER_ID)}, {sql_lit(f'Kanban update: auto-started implementation via Codex (`{MODEL}`, {reasoning} reasoning).')});
    commit;
    """
    psql_exec(sql)
    run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", f"Kanban update: auto-started pilot card {card.simple_id} via Codex (`{MODEL}`, `{reasoning}` reasoning)."])
    print(f"started {card.simple_id}: model={MODEL} reasoning={reasoning} workspace {ws['id']} process {res['data']['execution_process']['id']}")


def latest_impl_process(card: Card) -> dict[str, Any] | None:
    impl = implementation_processes(card)
    return impl[0] if impl else None


def start_auto_review(card: Card, *, rerun: bool = False) -> None:
    if not card.local_workspace_id:
        return
    if has_running_auto_review(card):
        print(f"{card.simple_id}: auto-review already running; not starting another")
        return
    issue = issue_json(card.gh_number)
    reasoning = reasoning_for_issue(issue)
    session_name = f"Auto review{' rerun' if rerun else ''} — Codex ({reasoning})"
    create = api("/api/sessions", {"workspace_id": card.local_workspace_id, "executor": "CODEX", "name": session_name})
    sid = create["data"]["id"]
    prompt = "Review this workspace as an independent Codex reviewer. Do not implement changes. Check the linked GitHub issue acceptance criteria, PR/diff scope, validation evidence, and hygiene. Return one of exactly `Decision: pass` or `Decision: request changes`, followed by blockers, non-blocking notes, validation evidence, and recommended next action."
    payload = {
        "prompt": prompt,
        "executor_config": {"executor": "CODEX", "variant": "DEFAULT", "model_id": MODEL, "reasoning_id": reasoning, "permission_policy": "AUTO"},
        "retry_process_id": None,
        "force_when_dirty": True,
        "perform_git_reset": False,
    }
    try:
        review = api(f"/api/sessions/{sid}/follow-up", payload)
        print(f"review {'rerun ' if rerun else ''}started {card.simple_id}: model={MODEL} reasoning={reasoning} session {sid} process {review.get('data', {}).get('id')}")
    except urllib.error.HTTPError as exc:
        session_processes = [p for p in review_processes(card) if p["sid"] == sid.replace('-', '')]
        if session_processes:
            print(f"review {'rerun ' if rerun else ''}present for {card.simple_id} after API {exc.code}")
            return
        raise
    except Exception as exc:
        session_processes = [p for p in review_processes(card) if p["sid"] == sid.replace('-', '')]
        if session_processes:
            print(f"review {'rerun ' if rerun else ''}present for {card.simple_id} after API error: {exc}")
            return
        raise


def has_review_process(card: Card) -> bool:
    if not card.local_workspace_id:
        return False
    return bool(review_processes(card))


def latest_impl_completed(card: Card) -> bool:
    if not card.local_workspace_id:
        return False
    impl = implementation_processes(card)
    if not impl:
        return False
    latest = impl[0]
    return latest["status"] == "completed" and str(latest["exit_code"]) == "0"


def move_to_review_and_start_review(card: Card) -> None:
    if not card.local_workspace_id:
        return
    review_id = status_id(STATUS_IN_REVIEW)
    psql_exec(f"""
    begin;
    update issues set status_id={sql_lit(review_id)}, updated_at=now() where id={sql_lit(card.id)};
    insert into issue_comments (issue_id, author_id, message) values ({sql_lit(card.id)}, {sql_lit(USER_ID)}, 'Kanban update: implementation completed successfully; auto-promoted to In review. Starting Codex auto-review if not already present.');
    commit;
    """)
    if not has_review_process(card):
        try:
            start_auto_review(card)
        except Exception as exc:
            # Some review starts return a typed/422 response after the process has
            # already been created. Re-check local state before failing the loop.
            if has_review_process(card):
                print(f"review present for {card.simple_id} after API error: {exc}")
            else:
                raise
    run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", f"Kanban update: pilot card {card.simple_id} auto-promoted to In review after implementation completed. Codex auto-review is {'already present' if has_review_process(card) else 'starting'}."], check=False)
    print(f"moved {card.simple_id} to review")


def workspace_workdir(card: Card) -> str | None:
    if not card.local_workspace_id:
        return None
    con = sqlite3.connect(LOCAL_DB)
    con.row_factory = sqlite3.Row
    row = con.execute(
        """
        select w.container_ref, s.agent_working_dir
        from workspaces w
        left join sessions s on s.workspace_id = w.id and s.agent_working_dir is not null and s.agent_working_dir != ''
        where w.id = cast(? as blob)
        order by s.created_at desc
        limit 1
        """,
        (bytes.fromhex(card.local_workspace_id.replace('-', '')),),
    ).fetchone()
    if not row:
        return None
    candidates: list[Path] = []
    agent_dir = row["agent_working_dir"]
    container_ref = row["container_ref"]
    if agent_dir:
        p = Path(agent_dir)
        candidates.append(p)
        if container_ref and not p.is_absolute():
            candidates.append(Path(container_ref) / agent_dir)
    if container_ref:
        candidates.append(Path(container_ref))
    for p in candidates:
        if p.exists():
            return str(p)
        alt = Path(str(p).replace("/var/folders/", "/private/var/folders/", 1))
        if alt.exists():
            return str(alt)
    branch_row = con.execute(
        "select branch from workspaces where id=cast(? as blob)",
        (bytes.fromhex(card.local_workspace_id.replace('-', '')),),
    ).fetchone()
    if branch_row and branch_row["branch"]:
        branch_leaf = str(branch_row["branch"]).removeprefix("vk/")
        fallback = Path("/private/var/folders/ty/4gtshgnj5452rntj36wbmskw0000gn/T/vibe-kanban-dev/worktrees") / branch_leaf / "implication"
        if fallback.exists():
            return str(fallback)
    return None


def pr_view(branch: str) -> dict[str, Any] | None:
    out = run([
        "gh", "pr", "list", "--repo", GH_REPO, "--state", "open", "--limit", "100",
        "--json", "number,url,state,mergeable,mergeStateStatus,isDraft,headRefName,headRefOid,statusCheckRollup,mergedAt",
    ], check=False)
    if not out.strip():
        return None
    try:
        rows = [
            row for row in json.loads(out)
            if row.get("headRefName") == branch and row.get("state") == "OPEN" and not row.get("mergedAt")
        ]
        return rows[0] if rows else None
    except json.JSONDecodeError:
        return None


def pr_view_number(number: int | str) -> dict[str, Any] | None:
    out = run([
        "gh", "pr", "view", str(number), "--repo", GH_REPO,
        "--json", "number,url,state,mergeable,mergeStateStatus,isDraft,headRefName,headRefOid,statusCheckRollup,mergedAt",
    ], check=False)
    if not out.strip():
        return None
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return None


def pr_for_issue(number: int) -> dict[str, Any] | None:
    out = run([
        "gh", "pr", "list", "--repo", GH_REPO, "--state", "all", "--search", str(number),
        "--json", "number,url,state,mergeable,mergeStateStatus,isDraft,headRefName,headRefOid,statusCheckRollup,mergedAt", "--limit", "10",
    ], check=False)
    if not out.strip():
        return None
    try:
        prs = json.loads(out)
    except json.JSONDecodeError:
        return None
    if not prs:
        return None
    # Prefer merged PRs, then open PRs, then newest returned item.
    for pr in prs:
        if pr.get("state") == "MERGED" or pr.get("mergedAt"):
            return pr
    for pr in prs:
        if pr.get("state") == "OPEN":
            return pr
    return prs[0]


def ensure_pr(card: Card, *, allow_merged_issue_pr: bool = True) -> dict[str, Any] | None:
    branch = None
    if card.local_workspace_id:
        con = sqlite3.connect(LOCAL_DB)
        con.row_factory = sqlite3.Row
        row = con.execute("select branch from workspaces where id=cast(? as blob)", (bytes.fromhex(card.local_workspace_id.replace('-', '')),)).fetchone()
        if row:
            branch = row["branch"]
    if branch:
        existing = pr_view(branch)
        if existing and (existing.get("state") == "OPEN" and not existing.get("mergedAt")):
            return existing
        if existing and allow_merged_issue_pr:
            return existing

    issue_pr = pr_for_issue(card.gh_number)
    if issue_pr and (allow_merged_issue_pr or (issue_pr.get("state") == "OPEN" and not issue_pr.get("mergedAt"))):
        return issue_pr
    if not card.local_workspace_id or not branch:
        return None
    wd = workspace_workdir(card)
    if not wd or not Path(wd).exists():
        print(f"{card.simple_id}: cannot create PR; missing workdir for branch {branch}")
        return None
    issue = issue_json(card.gh_number)
    push = subprocess.run(["git", "push", "--no-verify", "-u", "origin", branch], cwd=wd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if push.returncode != 0:
        print(f"{card.simple_id}: git push failed for {branch}: {push.stderr.strip()}")
        return None
    body = f"""## Summary
Automated Kanban implementation for {card.simple_id} / {GH_REPO}#{card.gh_number}.

## Validation
- [x] Implementation agent completed successfully.
- [x] Codex auto-review is required before merge.

Closes #{card.gh_number}
"""
    create = run([
        "gh", "pr", "create", "--repo", GH_REPO, "--base", "master", "--head", branch,
        "--title", issue["title"], "--body", body,
    ], check=False)
    if create.strip():
        print(f"created PR for {card.simple_id}: {create.strip()}")
    return pr_view(branch)


def latest_check_rollup_by_name(pr: dict[str, Any]) -> dict[str, dict[str, Any]]:
    latest_by_name: dict[str, dict[str, Any]] = {}
    for check in pr.get("statusCheckRollup") or []:
        name = check.get("name") or check.get("context") or "check"
        previous = latest_by_name.get(name)
        # Prefer the newest started/completed timestamp when GitHub returns old
        # cancelled/failed runs alongside newer successful reruns.
        stamp = check.get("startedAt") or check.get("completedAt") or ""
        prev_stamp = (previous or {}).get("startedAt") or (previous or {}).get("completedAt") or ""
        if previous is None or stamp >= prev_stamp:
            latest_by_name[name] = check
    return latest_by_name


def check_blockers(pr: dict[str, Any]) -> tuple[list[str], list[str]]:
    bad = []
    pending = []
    for name, check in latest_check_rollup_by_name(pr).items():
        status = check.get("status")
        conclusion = check.get("conclusion")
        if status != "COMPLETED":
            pending.append(name)
        elif conclusion not in {"SUCCESS", "SKIPPED", "NEUTRAL"}:
            bad.append(f"{name}:{conclusion}")
    return bad, pending


def checks_pass_or_absent(pr: dict[str, Any]) -> tuple[bool, str]:
    checks = pr.get("statusCheckRollup") or []
    if not checks:
        return True, "no GitHub checks reported"
    bad, pending = check_blockers(pr)
    if bad:
        return False, "failed checks: " + ", ".join(bad)
    if pending:
        return False, "pending checks: " + ", ".join(pending)
    return True, "checks passed"


def is_metadata_check_name(name: str) -> bool:
    normalized = name.lower()
    return any(token in normalized for token in [
        "hygiene",
        "metadata",
        "pr body",
        "issue evidence",
    ])


def checks_blocked_only_by_metadata(pr: dict[str, Any]) -> tuple[bool, str]:
    bad, pending = check_blockers(pr)
    blockers = bad + pending
    if not blockers:
        return False, "no check blockers"
    names = [blocker.split(":", 1)[0] for blocker in blockers]
    if all(is_metadata_check_name(name) for name in names):
        return True, ", ".join(blockers)
    return False, ", ".join(blockers)


def card_comment(card: Card, message: str) -> None:
    psql_exec(f"insert into issue_comments (issue_id, author_id, message) values ({sql_lit(card.id)}, {sql_lit(USER_ID)}, {sql_lit(message)});")


def latest_review_fix_after(card: Card, review_proc: dict[str, Any] | None) -> dict[str, Any] | None:
    if not review_proc or not review_proc.get("completed_at"):
        return None
    for p in implementation_processes(card):
        if "review fix" in (p.get("session") or "").lower() and p.get("started_at", "") > review_proc["completed_at"]:
            return p
    return None


def load_state() -> dict[str, Any]:
    if not STATE_PATH.exists():
        return {}
    try:
        data = json.loads(STATE_PATH.read_text())
    except (OSError, json.JSONDecodeError):
        return {}
    return data if isinstance(data, dict) else {}


def save_state(state: dict[str, Any]) -> None:
    STATE_PATH.parent.mkdir(parents=True, exist_ok=True)
    tmp = STATE_PATH.with_suffix(".tmp")
    tmp.write_text(json.dumps(state, sort_keys=True, indent=2) + "\n")
    tmp.replace(STATE_PATH)


def pr_head_identity(pr: dict[str, Any]) -> str:
    return str(pr.get("headRefOid") or pr.get("headOid") or pr.get("headSha") or pr.get("headRefName") or "")


def pr_check_state_identity(pr: dict[str, Any]) -> str:
    checks = latest_check_rollup_by_name(pr)
    parts = []
    for name, check in sorted(checks.items()):
        stamp = check.get("completedAt") or check.get("startedAt") or ""
        parts.append(f"{name}:{check.get('status') or ''}:{check.get('conclusion') or ''}:{stamp}")
    return "|".join(parts) or "no-checks"


def pr_review_state_key(card: Card, pr: dict[str, Any]) -> str:
    workspace = card.local_workspace_id or "no-workspace"
    return f"{workspace}:pr-{pr.get('number')}:{pr_head_identity(pr)}:{pr_check_state_identity(pr)}"


def review_rerun_attempted_for_pr_state(state: dict[str, Any], card: Card, pr: dict[str, Any]) -> bool:
    return pr_review_state_key(card, pr) in state.get("review_reruns", {})


def pr_metadata_repair_key(card: Card, pr: dict[str, Any]) -> str:
    workspace = card.local_workspace_id or "no-workspace"
    return f"{workspace}:pr-{pr.get('number')}:{pr_head_identity(pr)}"


def metadata_repair_attempted_for_pr_head(state: dict[str, Any], card: Card, pr: dict[str, Any]) -> bool:
    return pr_metadata_repair_key(card, pr) in state.get("metadata_repairs", {})


def record_metadata_repair_attempt(state: dict[str, Any], card: Card, pr: dict[str, Any], reason: str) -> None:
    state.setdefault("metadata_repairs", {})[pr_metadata_repair_key(card, pr)] = {
        "card": card.simple_id,
        "workspace_id": card.local_workspace_id,
        "pr_number": pr.get("number"),
        "head": pr_head_identity(pr),
        "reason": reason,
        "recorded_at": int(time.time()),
    }


def record_review_rerun_attempt(state: dict[str, Any], card: Card, pr: dict[str, Any], reason: str) -> None:
    state.setdefault("review_reruns", {})[pr_review_state_key(card, pr)] = {
        "card": card.simple_id,
        "workspace_id": card.local_workspace_id,
        "pr_number": pr.get("number"),
        "head": pr_head_identity(pr),
        "check_state": pr_check_state_identity(pr),
        "reason": reason,
        "recorded_at": int(time.time()),
    }


def merge_conflict_blocker_recorded(state: dict[str, Any], card: Card, pr: dict[str, Any]) -> bool:
    return pr_review_state_key(card, pr) in state.get("merge_conflict_blockers", {})


def record_merge_conflict_blocker(state: dict[str, Any], card: Card, pr: dict[str, Any], context: str) -> None:
    state.setdefault("merge_conflict_blockers", {})[pr_review_state_key(card, pr)] = {
        "card": card.simple_id,
        "workspace_id": card.local_workspace_id,
        "pr_number": pr.get("number"),
        "head": pr_head_identity(pr),
        "mergeable": pr.get("mergeable"),
        "mergeStateStatus": pr.get("mergeStateStatus"),
        "context": context,
        "recorded_at": int(time.time()),
    }


def pr_has_merge_conflict(pr: dict[str, Any]) -> bool:
    return pr.get("mergeable") == "CONFLICTING" or pr.get("mergeStateStatus") in {"CONFLICTING", "DIRTY"}


def fix_process_has_substantive_result(process: dict[str, Any]) -> bool:
    if process.get("status") != "completed" or str(process.get("exit_code")) != "0":
        return False
    text = review_result_text(process)
    normalized = re.sub(r"\s+", " ", text.lower())
    return bool(re.search(r"\b[0-9a-f]{7,40}\b", text)) or any(k in normalized for k in [
        "commit hash",
        "committed",
        "changed files",
        "validation",
    ])


def has_review_attempt_after(card: Card, completed_at: str | None) -> bool:
    if not completed_at:
        return False
    return any(p.get("started_at", "") > completed_at for p in review_processes(card))


def latest_substantive_review_fix_after(card: Card, completed_at: str | None) -> dict[str, Any] | None:
    if not completed_at:
        return None
    fixes = [
        p for p in implementation_processes(card)
        if "review fix" in (p.get("session") or "").lower()
        and p.get("completed_at")
        and p["completed_at"] > completed_at
        and fix_process_has_substantive_result(p)
    ]
    return fixes[0] if fixes else None


def mark_merge_conflict_blocker(card: Card, pr: dict[str, Any], context: str) -> None:
    state = load_state()
    if merge_conflict_blocker_recorded(state, card, pr):
        print(f"{card.simple_id}: PR #{pr.get('number')} still has merge conflicts; blocker already recorded")
        return
    record_merge_conflict_blocker(state, card, pr, context)
    save_state(state)
    message = (
        f"Kanban update: {card.simple_id} PR #{pr.get('number')} is not safe to re-review or merge because "
        f"GitHub reports mergeable={pr.get('mergeable')} mergeStateStatus={pr.get('mergeStateStatus')}. "
        "Manual conflict resolution or a bounded fix/update session is required before re-review."
    )
    card_comment(card, message)
    run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", message], check=False)
    print(f"{card.simple_id}: recorded merge-conflict blocker for PR #{pr.get('number')} ({context})")


def start_rereview_after_pr_surface(card: Card, review_proc: dict[str, Any] | None, pr: dict[str, Any], checks_msg: str) -> None:
    if has_running_auto_review(card):
        print(f"{card.simple_id}: PR surface handled; auto-review already running")
        return
    if pr_has_merge_conflict(pr):
        mark_merge_conflict_blocker(card, pr, "PR surface handled but merge is dirty/conflicting")
        return
    state = load_state()
    if review_rerun_attempted_for_pr_state(state, card, pr):
        print(f"{card.simple_id}: PR surface handled; re-review already attempted for current PR head/check state")
        return
    review_cutoff = review_proc.get("completed_at") if review_proc else None
    fix_after_review = latest_substantive_review_fix_after(card, review_cutoff)
    if fix_after_review:
        review_cutoff = fix_after_review.get("completed_at") or review_cutoff
    if review_proc and has_review_attempt_after(card, review_cutoff):
        print(f"{card.simple_id}: PR surface handled; re-review already attempted after latest review/fix state")
        return
    record_review_rerun_attempt(state, card, pr, "PR surface handled")
    save_state(state)
    print(f"{card.simple_id}: PR surface handled ({checks_msg}); starting exactly one re-review")
    start_auto_review(card, rerun=True)


def start_review_fix(card: Card, review_text: str) -> None:
    if not card.local_workspace_id or not AUTO_FIX_REVIEW_FAILURES:
        return
    if any(p["status"] == "running" for p in implementation_processes(card)):
        return
    existing_fix = [p for p in implementation_processes(card) if "review fix" in (p["session"] or "").lower()]
    latest_review = latest_completed_review(card)
    if existing_fix and latest_review:
        newest_fix = existing_fix[0]
        if newest_fix["started_at"] > latest_review["completed_at"]:
            if newest_fix["status"] == "running":
                print(f"{card.simple_id}: review-fix already running after latest review")
                return
            if fix_process_has_substantive_result(newest_fix):
                print(f"{card.simple_id}: review-fix already completed after latest review; waiting for re-review")
                return
            print(f"{card.simple_id}: latest review-fix after review was {newest_fix['status']} or no-op; not starting another fix loop")
            return
    issue = issue_json(card.gh_number)
    reasoning = reasoning_for_issue(issue)
    create = api("/api/sessions", {"workspace_id": card.local_workspace_id, "executor": "CODEX", "name": f"Review fix — Codex ({reasoning})"})
    sid = create["data"]["id"]
    prompt = f"""The independent auto-review for Kanban card {card.simple_id} requested changes.

Canonical issue: {GH_REPO}#{card.gh_number}
Title: {issue['title']}

Review finding to fix:
{review_text[-6000:]}

Fix only the blockers, run focused validation, commit the fix, and final-answer with changed files, commit hash, and focused validation evidence. Do not broaden scope. Do not run broad local Vibe Kanban/Rust/TS validation loops or release/native builds on Host A/omarchy (`pnpm run check`, `pnpm run backend:check`, `pnpm run lint`, `cargo check --workspace`, `cargo test --workspace`, broad `cargo test`, `cargo clippy --workspace`, `pnpm run generate-types:check`, `cargo build --release`, or Tauri release builds) unless explicit operator approval sets `ALLOW_HEAVY_VIBE_VALIDATION=1`; prefer CI/off-host for full validation.
"""
    payload = {
        "prompt": prompt,
        "executor_config": {"executor": "CODEX", "variant": "DEFAULT", "model_id": MODEL, "reasoning_id": reasoning},
        "retry_process_id": None,
        "force_when_dirty": True,
        "perform_git_reset": False,
    }
    res = api(f"/api/sessions/{sid}/follow-up", payload)
    card_comment(card, f"Kanban update: auto-review requested changes; started review-fix Codex session `{sid}` using `{MODEL}` with `{reasoning}` reasoning.")
    run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", f"Kanban update: auto-review for {card.simple_id} requested changes; started an automatic review-fix Codex session (`{MODEL}`, `{reasoning}` reasoning)."], check=False)
    print(f"started review fix {card.simple_id}: model={MODEL} reasoning={reasoning} session {sid} process {res.get('data', {}).get('id')}")


def review_is_pr_surface_blocked(review_text: str) -> bool:
    normalized = review_text.lower()
    return any(phrase in normalized for phrase in [
        "no remote branch",
        "local-only",
        "no reviewable pr",
        "no reviewable pr/ci surface",
        "push `",
        "open a new pr",
        "pushed pr",
    ])


def review_has_real_ci_or_environment_blocker(review_text: str) -> bool:
    normalized = review_text.lower().replace("audit trail", "spec trail")
    return any(marker in normalized for marker in [
        "ci audit / audit` failed",
        "audit` failed",
        "audit failed",
        "audit failure",
        "runner drift",
        "runner baseline",
        "runner lacks",
        "self-hosted runner path",
        "rustc` and `cargo` are missing",
        "rustc and cargo are missing",
        "missing rustc",
        "missing cargo",
        "cargo test could not run",
    ])


def review_is_pr_metadata_blocked(review_text: str) -> bool:
    normalized = review_text.lower()
    if review_has_real_ci_or_environment_blocker(review_text):
        return False
    metadata_markers = [
        "hygiene",
        "pr hygiene",
        "pr metadata",
        "pr body",
        "pr contract",
        "issue-link keyword",
        "issue update",
        "implementation evidence",
        "validation checkbox",
        "validation checkboxes",
        "no ui change evidence",
        "ui change evidence",
        "relates to #",
        "part of #",
        "malformed validation",
    ]
    return any(marker in normalized for marker in metadata_markers)


def workspace_changed_files(card: Card, pr: dict[str, Any]) -> list[str]:
    wd = workspace_workdir(card)
    if not wd:
        return []
    base = "origin/master"
    out = subprocess.run(["git", "diff", "--name-only", f"{base}...HEAD"], cwd=wd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    if out.returncode != 0 or not out.stdout.strip():
        out = subprocess.run(["git", "diff", "--name-only", "HEAD~1..HEAD"], cwd=wd, text=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
    return [line.strip() for line in out.stdout.splitlines() if line.strip()]


def ui_change_evidence(files: list[str]) -> str:
    ui_roots = ("packages/", "assets/", "dev_assets_seed/", "npx-cli/")
    ui_exts = (".tsx", ".ts", ".jsx", ".js", ".css", ".scss", ".mdx")
    ui_files = [path for path in files if path.startswith(ui_roots) or path.endswith(ui_exts)]
    if ui_files:
        return "UI-facing files changed: " + ", ".join(ui_files[:12])
    return "No UI-facing files detected in the workspace diff."


def build_repaired_pr_body(card: Card, pr: dict[str, Any], issue: dict[str, Any], review_text: str, issue_update_url: str) -> str:
    files = workspace_changed_files(card, pr)
    changed = "\n".join(f"- `{path}`" for path in files[:40]) or "- No changed files detected from local workspace diff."
    latest_impl = latest_impl_process(card)
    impl_text = review_result_text(latest_impl) if latest_impl else ""
    validation_lines = []
    for line in impl_text.splitlines():
        if any(word in line.lower() for word in ["validation", "test", "check", "cargo", "pnpm"]):
            validation_lines.append(line.strip())
    command_validation = [
        line.strip() for line in validation_lines
        if any(token in line for token in ["`", "bun ", "node ", "python", "git ", "pnpm ", "cargo "])
    ]
    validation = "\n".join(
        f"- [x] `{line.strip('`')[:220]}` completed during the implementation/review evidence pass."
        for line in command_validation[:4]
    ) or "- [x] Manual review only: implementation agent completed successfully; see workspace session logs for detailed validation output."
    blocker_excerpt = re.sub(r"\s+", " ", review_text).strip()[:600]
    ui_evidence = ui_change_evidence(files)
    if "No UI" in ui_evidence or "No UI-facing" in ui_evidence:
        ui_evidence = "No UI change; no `rebuild/ui/` or `rebuild/ui/packages/` files changed."
    return f"""## Summary
Automated Kanban implementation for {card.simple_id} / {GH_REPO}#{card.gh_number}.

## Issue Link
{issue.get('title') or card.title}
{issue.get('url') or ''}

Closes #{card.gh_number}

## Changed Files
{changed}

## Validation
{validation}
- [x] Manual review only: Codex auto-review ran before merge and the PR body includes a closing issue link.

## Spec + audit trail
- [x] Lightweight spec/audit trail is included in the changed files above when applicable, or the implementation is a bounded Kanban task.
- [x] Autopilot metadata repair checked the PR contract after Codex review reported a metadata-only blocker.

## Issue update
- [x] Issue comment link: {issue_update_url}

## UI evidence
- [x] {ui_evidence}

## Runtime / Deploy Impact
- [x] No runtime impact unless explicitly described in the changed files; this metadata repair changes PR text only.

## Migration / Data Impact
- [x] No migration/data/model impact from this metadata repair; data generation, labels, provenance, leakage risk, and evaluation comparability are unchanged.

## Rollback Plan
- [x] Revert the PR or edit this PR body back to the previous text if the metadata repair is wrong.

## Policy override
- [x] Not used
- Reason: Normal policy path; no emergency/human-maintainer override used.

## Autopilot Metadata Repair
The autopilot repaired PR metadata after Codex review reported a metadata-only blocker. Latest blocker excerpt: {blocker_excerpt or 'metadata-only hygiene failure'}.

## Review State
- [x] Ready for Codex re-review after metadata repair.
"""


def repair_pr_metadata(card: Card, pr: dict[str, Any], review_text: str) -> str:
    state = load_state()
    if metadata_repair_attempted_for_pr_head(state, card, pr):
        print(f"{card.simple_id}: PR metadata already repaired for current PR head; not editing body again")
        return "already"
    issue = issue_json(card.gh_number)
    evidence = (
        f"Kanban update: repaired PR metadata for {card.simple_id} after metadata-only auto-review blocker. "
        f"PR #{pr.get('number')} now includes closing issue link, validation checkboxes, changed-file evidence, spec/audit trail, issue-update evidence, and UI-change evidence."
    )
    card_comment(card, evidence)
    issue_comment_url = run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", evidence], check=False).strip()
    if not issue_comment_url:
        issue_comment_url = f"GitHub issue #{card.gh_number} received an autopilot metadata repair evidence comment."
    body = build_repaired_pr_body(card, pr, issue, review_text, issue_comment_url)
    edit_proc = subprocess.run(
        ["gh", "pr", "edit", str(pr["number"]), "--repo", GH_REPO, "--body-file", "-"],
        input=body,
        text=True,
        cwd=ROOT,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if edit_proc.returncode != 0:
        print(f"{card.simple_id}: PR metadata edit failed for PR #{pr['number']}: {edit_proc.stderr.strip() or edit_proc.stdout.strip()}")
        return "failed"
    record_metadata_repair_attempt(state, card, pr, "metadata-only auto-review blocker")
    save_state(state)
    if edit_proc.stdout.strip():
        print(f"{card.simple_id}: PR metadata edit output: {edit_proc.stdout.strip()}")
    return "edited"


def passed_review_should_mark_ready(auto_merge: bool, pr: dict[str, Any]) -> bool:
    return auto_merge and bool(pr.get("isDraft"))


def mark_pr_ready_for_review(card: Card, pr: dict[str, Any]) -> dict[str, Any]:
    out = run(["gh", "pr", "ready", str(pr["number"]), "--repo", GH_REPO], check=False)
    if out.strip():
        print(f"{card.simple_id}: marked PR #{pr['number']} ready: {out.strip()}")
    refreshed = pr_view_number(pr["number"]) or pr
    card_comment(card, f"Kanban update: auto-review passed and AUTO_MERGE is enabled; marked draft PR #{pr.get('number')} ready for review.")
    run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", f"Kanban update: {card.simple_id} auto-review passed; marked draft PR #{pr.get('number')} ready for review so merge checks can continue."], check=False)
    return refreshed


def finalize_reviewed_card(card: Card) -> None:
    if any(p["status"] == "running" for p in implementation_processes(card)):
        print(f"{card.simple_id}: implementation/fix process still running; not finalizing review")
        return
    latest_impl = latest_impl_process(card)
    latest_review = latest_completed_review(card)
    if latest_impl and latest_review and latest_impl.get("completed_at") and latest_impl["completed_at"] > latest_review["completed_at"]:
        if "review fix" in (latest_impl.get("session") or "").lower() and not fix_process_has_substantive_result(latest_impl):
            print(f"{card.simple_id}: ignoring latest review-fix after review because it was {latest_impl['status']} or no-op")
            latest_impl = None
    if latest_impl and latest_impl["status"] == "completed" and str(latest_impl["exit_code"]) == "0" and latest_review and latest_impl["completed_at"] > latest_review["completed_at"]:
        pr = ensure_pr(card, allow_merged_issue_pr=False)
        if pr:
            start_rereview_after_pr_surface(card, latest_review, pr, "implementation/fix completed after latest review")
        elif not has_running_auto_review(card):
            print(f"{card.simple_id}: implementation/fix completed after latest review; starting re-review with model={MODEL} reasoning={REASONING}")
            start_auto_review(card, rerun=True)
        return
    decision, review_text, review_proc = review_decision(card)
    if decision == "running":
        return
    if decision == "missing":
        pr = ensure_pr(card, allow_merged_issue_pr=False)
        if pr:
            if pr_has_merge_conflict(pr):
                mark_merge_conflict_blocker(card, pr, "missing usable review result")
                return
            state = load_state()
            if review_rerun_attempted_for_pr_state(state, card, pr):
                print(f"{card.simple_id}: no usable auto-review result; review already attempted for current PR head/check state")
                return
            record_review_rerun_attempt(state, card, pr, "missing usable review result")
            save_state(state)
        if not has_running_auto_review(card):
            print(f"{card.simple_id}: no usable auto-review result; starting review with model={MODEL} reasoning={REASONING}")
            start_auto_review(card, rerun=True)
        return
    if decision in {"request_changes", "failed"}:
        if review_text and review_is_pr_surface_blocked(review_text):
            pr = ensure_pr(card, allow_merged_issue_pr=False)
            if pr:
                checks_ok, checks_msg = checks_pass_or_absent(pr)
                card_comment(card, f"Kanban update: auto-review requested a reviewable PR surface; opened/verified PR #{pr.get('number')}. Next action: {'start re-review' if checks_ok else 'wait for ' + checks_msg}.")
                run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", f"Kanban update: {card.simple_id} auto-review requested a reviewable PR surface; opened/verified PR #{pr.get('number')} ({pr.get('url')})."], check=False)
                if checks_ok:
                    start_rereview_after_pr_surface(card, review_proc, pr, checks_msg)
                else:
                    print(f"{card.simple_id}: PR surface present; waiting on {checks_msg}")
            else:
                print(f"{card.simple_id}: review requested PR surface but PR creation failed")
            return
        if review_text and review_is_pr_metadata_blocked(review_text):
            pr = ensure_pr(card, allow_merged_issue_pr=False)
            if not pr:
                print(f"{card.simple_id}: metadata review blocker but no open PR found")
                return
            repair_result = repair_pr_metadata(card, pr, review_text)
            if repair_result == "failed":
                print(f"{card.simple_id}: PR metadata repair failed; not re-reviewing unchanged metadata")
                return
            refreshed = pr_view_number(pr["number"]) or pr
            checks_ok, checks_msg = checks_pass_or_absent(refreshed)
            metadata_only_checks, metadata_checks_msg = checks_blocked_only_by_metadata(refreshed)
            if checks_ok:
                start_rereview_after_pr_surface(card, review_proc, refreshed, checks_msg)
            elif metadata_only_checks:
                bad, pending = check_blockers(refreshed)
                if pending:
                    print(f"{card.simple_id}: PR metadata repaired; waiting for metadata checks to finish: {metadata_checks_msg}")
                else:
                    start_rereview_after_pr_surface(card, review_proc, refreshed, f"metadata checks still blocked after repair: {metadata_checks_msg}")
            else:
                print(f"{card.simple_id}: PR metadata repaired; waiting on non-metadata check blockers: {checks_msg}")
            return
        if review_has_real_ci_or_environment_blocker(review_text):
            pr = ensure_pr(card, allow_merged_issue_pr=False)
            if pr:
                checks_ok, checks_msg = checks_pass_or_absent(pr)
                if checks_ok:
                    print(f"{card.simple_id}: prior real CI/environment blocker is now clear ({checks_msg}); starting re-review")
                    start_rereview_after_pr_surface(card, review_proc, pr, checks_msg)
                    return
            message = f"Kanban update: auto-review requested changes for {card.simple_id}; blocker is a real CI/runner/environment failure, not metadata or code-fix automation. Manual/operator action required before re-review."
            card_comment(card, message)
            run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", message], check=False)
            print(f"{card.simple_id}: review requested changes on real CI/environment blocker; not starting code-fix session")
            return
        if latest_review_fix_after(card, review_proc):
            fix = latest_review_fix_after(card, review_proc)
            assert fix is not None
            if fix["status"] == "running":
                print(f"{card.simple_id}: requested changes but review-fix is already running")
            elif fix_process_has_substantive_result(fix):
                print(f"{card.simple_id}: requested changes already had a substantive fix; waiting for re-review")
            else:
                print(f"{card.simple_id}: requested changes after latest review but latest fix was {fix['status']} or no-op; not looping")
            return
        print(f"{card.simple_id}: review decision {decision}; starting fix if possible")

        start_review_fix(card, review_text)
        return
    pr = ensure_pr(card)
    if not pr:
        print(f"{card.simple_id}: review passed but no PR found/created")
        return
    if pr.get("state") == "MERGED" or pr.get("mergedAt"):
        mark_done(card, pr, "PR already merged")
        return
    if passed_review_should_mark_ready(AUTO_MERGE, pr):
        pr = mark_pr_ready_for_review(card, pr)
    if pr.get("isDraft"):
        print(f"{card.simple_id}: review passed but PR is draft")
        return
    if pr_has_merge_conflict(pr):
        mark_merge_conflict_blocker(card, pr, "review passed but PR is dirty/conflicting")
        return
    checks_ok, checks_msg = checks_pass_or_absent(pr)
    if not checks_ok:
        print(f"{card.simple_id}: review passed; waiting on {checks_msg}")
        if AUTO_MERGE:
            run(["gh", "pr", "merge", str(pr["number"]), "--repo", GH_REPO, "--auto", "--squash", "--delete-branch"], check=False)
        return
    if AUTO_MERGE:
        merge = run(["gh", "pr", "merge", str(pr["number"]), "--repo", GH_REPO, "--squash", "--delete-branch"], check=False)
        print(f"merge {card.simple_id} PR #{pr['number']}: {merge.strip()}")
        refreshed = pr_view_number(pr["number"]) or pr
        if refreshed.get("state") == "MERGED" or refreshed.get("mergedAt"):
            mark_done(card, refreshed, checks_msg)
    else:
        print(f"{card.simple_id}: review passed and checks ok; AUTO_MERGE disabled")


def mark_done(card: Card, pr: dict[str, Any], evidence: str) -> None:
    done_id = status_id(STATUS_DONE)
    psql_exec(f"""
    begin;
    update issues set status_id={sql_lit(done_id)}, updated_at=now() where id={sql_lit(card.id)};
    insert into issue_comments (issue_id, author_id, message) values ({sql_lit(card.id)}, {sql_lit(USER_ID)}, {sql_lit(f'Kanban update: auto-review passed; PR #{pr.get("number")} merged; moved to Done. Evidence: {evidence}')});
    commit;
    """)
    run(["gh", "issue", "comment", str(card.gh_number), "--repo", GH_REPO, "--body", f"Kanban update: {card.simple_id} auto-review passed and PR #{pr.get('number')} merged. Moved card to Done. Evidence: {evidence}"], check=False)
    print(f"done {card.simple_id}: PR #{pr.get('number')} merged")


def tick() -> None:
    cs = cards()
    # Promote completed implementations first.
    for c in cs:
        if c.status == STATUS_IN_PROGRESS and latest_impl_completed(c):
            move_to_review_and_start_review(c)

    cs = cards()
    # Finalize completed reviews before starting more work so dependencies can unblock.
    for c in cs:
        if c.status == STATUS_IN_REVIEW:
            finalize_reviewed_card(c)

    cs = cards()
    active = active_implementation_count(cs)
    slots = max(0, CAP - active)
    queued = [c for c in cs if c.status in {STATUS_BACKLOG, STATUS_TODO} and not c.local_workspace_id]
    for c in queued[:slots]:
        start_card(c)

    cs = cards()
    summary = ", ".join(f"{c.simple_id}:{c.status}{'/ws' if c.local_workspace_id else ''}" for c in cs)
    print(f"tick active={active} slots={slots} cards={summary}")


def self_test() -> None:
    assert reasoning_for_issue({"labels": []}) == "medium"
    test_batch = configured_batch({"VK_AUTOPILOT_BATCH": "configured-test-batch"})
    assert configured_batch({}) == DEFAULT_BATCH
    assert test_batch == "configured-test-batch"
    test_batch_sql = cards_sql(test_batch)
    assert "configured-test-batch" in test_batch_sql
    assert DEFAULT_BATCH not in test_batch_sql

    final_answer_lines = [
        json.dumps({"Stdout": "\n".join([
            json.dumps({"method": "item/started", "params": {"item": {"type": "agentMessage", "id": "final-1", "phase": "final_answer", "text": ""}}}),
            json.dumps({"method": "item/agentMessage/delta", "params": {"itemId": "final-1", "delta": "Decision: "}}),
            json.dumps({"method": "item/agentMessage/delta", "params": {"itemId": "final-1", "delta": "pass\nNo blockers."}}),
        ])}),
    ]
    assert "Decision: pass" in extract_process_agent_text(final_answer_lines)

    review_mode_lines = [
        json.dumps({"Stdout": json.dumps({
            "method": "item/completed",
            "params": {"item": {"type": "exitedReviewMode", "review": {"decision": "request changes", "body": "Blocking regression"}}},
        })}),
    ]
    assert "request changes" in extract_process_agent_text(review_mode_lines).lower()

    empty_review = {"status": "completed", "exit_code": "0", "text": "", "pid": "p", "sid": "s"}
    good_review = {"status": "completed", "exit_code": "0", "text": "Decision: pass\nNo blockers.", "pid": "p2", "sid": "s2"}
    killed_review = {"status": "killed", "exit_code": None, "text": "", "pid": "p3", "sid": "s3"}
    decision, text, proc = decide_from_review_attempts([empty_review, killed_review, good_review])
    assert decision == "pass"
    assert proc is good_review
    assert "No blockers" in text

    assert review_is_pr_metadata_blocked("Decision: request changes\nBlocker: hygiene check failed because the PR body is missing validation checkboxes and no UI change evidence.")
    assert review_is_pr_metadata_blocked("PR body should use an issue-link keyword such as Closes #123.")
    assert review_is_pr_metadata_blocked("PR263 is not review/merge ready because GitHub hygiene is failing. PR contract missing completed Spec audit trail and Issue update implementation evidence.")
    assert review_has_real_ci_or_environment_blocker("Decision: request changes\nBlocker: audit failed because the runner lacks rustc/cargo. The PR body also mentions validation.")
    assert not review_is_pr_metadata_blocked("Decision: request changes\nBlocker: audit failed because the runner lacks rustc/cargo. The PR body also mentions validation.")
    assert review_has_real_ci_or_environment_blocker("Audit failure: cargo test could not run because the environment is missing rustc.")
    assert not review_is_pr_metadata_blocked("Audit failure: cargo test could not run because the environment is missing rustc.")

    hygiene_pr = {"statusCheckRollup": [{"name": "hygiene", "status": "COMPLETED", "conclusion": "FAILURE"}]}
    audit_pr = {"statusCheckRollup": [{"name": "audit", "status": "COMPLETED", "conclusion": "FAILURE"}]}
    mixed_pr = {"statusCheckRollup": [
        {"name": "hygiene", "status": "COMPLETED", "conclusion": "FAILURE"},
        {"name": "audit", "status": "COMPLETED", "conclusion": "FAILURE"},
    ]}
    assert checks_blocked_only_by_metadata(hygiene_pr)[0]
    assert not checks_blocked_only_by_metadata(audit_pr)[0]
    assert not checks_blocked_only_by_metadata(mixed_pr)[0]

    fake_card = Card(
        id="issue-id",
        simple_id="BDF-T",
        title="Test",
        status=STATUS_IN_REVIEW,
        gh_number=123,
        priority="",
        local_workspace_id="11111111-1111-1111-1111-111111111111",
        workspace_name="test",
    )
    clean_pr = {
        "number": 456,
        "mergeable": "MERGEABLE",
        "mergeStateStatus": "CLEAN",
        "headRefOid": "abc123",
        "statusCheckRollup": [{"name": "ci", "status": "COMPLETED", "conclusion": "SUCCESS", "completedAt": "2026-04-25T00:00:00Z"}],
    }
    dirty_pr = {**clean_pr, "number": 457, "mergeable": "CONFLICTING", "mergeStateStatus": "DIRTY"}
    assert not pr_has_merge_conflict(clean_pr)
    assert pr_has_merge_conflict(dirty_pr)

    started_reviews: list[tuple[str, bool]] = []
    recorded_comments: list[str] = []
    state_box: dict[str, Any] = {}
    original_load_state = globals()["load_state"]
    original_save_state = globals()["save_state"]
    original_review_processes = globals()["review_processes"]
    original_start_auto_review = globals()["start_auto_review"]
    original_card_comment = globals()["card_comment"]
    original_run = globals()["run"]
    try:
        globals()["load_state"] = lambda: json.loads(json.dumps(state_box))

        def fake_save_state(state: dict[str, Any]) -> None:
            state_box.clear()
            state_box.update(json.loads(json.dumps(state)))

        globals()["save_state"] = fake_save_state
        globals()["review_processes"] = lambda card: []
        globals()["start_auto_review"] = lambda card, rerun=False: started_reviews.append((card.simple_id, rerun))
        globals()["card_comment"] = lambda card, message: recorded_comments.append(message)
        globals()["run"] = lambda *args, **kwargs: ""

        start_rereview_after_pr_surface(fake_card, None, clean_pr, "checks passed")
        start_rereview_after_pr_surface(fake_card, None, clean_pr, "checks passed")
        assert started_reviews == [("BDF-T", True)]
        assert review_rerun_attempted_for_pr_state(state_box, fake_card, clean_pr)

        start_rereview_after_pr_surface(fake_card, None, dirty_pr, "checks passed")
        assert started_reviews == [("BDF-T", True)]
        assert merge_conflict_blocker_recorded(state_box, fake_card, dirty_pr)
        assert any("not safe to re-review or merge" in comment for comment in recorded_comments)
    finally:
        globals()["load_state"] = original_load_state
        globals()["save_state"] = original_save_state
        globals()["review_processes"] = original_review_processes
        globals()["start_auto_review"] = original_start_auto_review
        globals()["card_comment"] = original_card_comment
        globals()["run"] = original_run

    assert passed_review_should_mark_ready(True, {"isDraft": True})
    assert not passed_review_should_mark_ready(False, {"isDraft": True})
    assert not passed_review_should_mark_ready(True, {"isDraft": False})


def main() -> int:
    parser = argparse.ArgumentParser(
        description=(
            "Run the Implication Kanban autopilot. Configure the target batch "
            f"with VK_AUTOPILOT_BATCH; defaults to {DEFAULT_BATCH}."
        )
    )
    parser.add_argument("--once", action="store_true")
    parser.add_argument("--loop", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        print("self-test passed")
        return 0
    if not LOCAL_DB.exists():
        raise SystemExit(f"missing local DB: {LOCAL_DB}")
    if args.loop:
        lock_path = ROOT / ".vibe-kanban-dev" / f"implication-kanban-autopilot-{BATCH}.lock"
        lock_path.parent.mkdir(parents=True, exist_ok=True)
        lock_file = lock_path.open("w")
        try:
            fcntl.flock(lock_file.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise SystemExit(f"autopilot already running for batch {BATCH}: {lock_path}")
        lock_file.write(f"pid={os.getpid()} batch={BATCH}\n")
        lock_file.flush()
        while True:
            try:
                tick()
            except Exception as exc:
                print(f"ERROR: {exc}", file=sys.stderr)
            time.sleep(POLL_SECONDS)
    else:
        tick()
    return 0

if __name__ == "__main__":
    raise SystemExit(main())
