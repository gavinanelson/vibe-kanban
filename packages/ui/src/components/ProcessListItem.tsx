import {
  TerminalIcon,
  GearIcon,
  CodeIcon,
  GlobeIcon,
} from '@phosphor-icons/react';
import { cn } from '../lib/cn';
import { RunningDots } from './RunningDots';

interface ProcessListItemProps {
  runReason: string;
  status: string;
  startedAt: string;
  completedAt?: string | null;
  executorLabel?: string | null;
  exitCode?: string | number | bigint | null;
  resultSummary?: string | null;
  selected?: boolean;
  onClick?: () => void;
  className?: string;
}

function getRunReasonLabel(runReason: string): string {
  switch (runReason) {
    case 'codingagent':
      return 'Coding Agent';
    case 'setupscript':
      return 'Setup Script';
    case 'cleanupscript':
      return 'Cleanup Script';
    case 'archivescript':
      return 'Archive Script';
    case 'devserver':
      return 'Dev Server';
    default:
      return runReason;
  }
}

function getRunReasonIcon(runReason: string): typeof TerminalIcon {
  switch (runReason) {
    case 'codingagent':
      return CodeIcon;
    case 'setupscript':
    case 'cleanupscript':
    case 'archivescript':
      return GearIcon;
    case 'devserver':
      return GlobeIcon;
    default:
      return TerminalIcon;
  }
}

function getStatusColor(status: string): string {
  switch (status) {
    case 'running':
      return 'bg-info/10 text-info';
    case 'completed':
      return 'bg-success/10 text-success';
    case 'failed':
      return 'bg-destructive/10 text-destructive';
    case 'killed':
      return 'bg-tertiary text-low';
    default:
      return 'bg-tertiary text-low';
  }
}

function getStatusLabel(status: string): string {
  switch (status) {
    case 'running':
      return 'Running';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    case 'killed':
      return 'Killed';
    default:
      return status;
  }
}

function formatRelativeElapsed(dateString: string | null | undefined): string {
  if (!dateString) {
    return '';
  }

  const date = new Date(dateString);
  if (Number.isNaN(date.getTime())) {
    return '';
  }

  const now = new Date();
  const diffMs = now.getTime() - date.getTime();
  const diffSecs = Math.floor(diffMs / 1000);
  const diffMins = Math.floor(diffSecs / 60);
  const diffHours = Math.floor(diffMins / 60);
  const diffDays = Math.floor(diffHours / 24);

  if (diffSecs < 60) return 'just now';
  if (diffMins < 60) return `${diffMins}m ago`;
  if (diffHours < 24) return `${diffHours}h ago`;
  return `${diffDays}d ago`;
}

export function ProcessListItem({
  runReason,
  status,
  startedAt,
  completedAt,
  executorLabel,
  exitCode,
  resultSummary,
  selected,
  onClick,
  className,
}: ProcessListItemProps) {
  const IconComponent = getRunReasonIcon(runReason);
  const label = getRunReasonLabel(runReason);
  const statusColor = getStatusColor(status);
  const statusLabel = getStatusLabel(status);

  const isRunning = status === 'running';
  const elapsedLabel = formatRelativeElapsed(
    isRunning ? startedAt : completedAt || startedAt
  );
  const exitLabel =
    !isRunning && exitCode !== null && exitCode !== undefined
      ? `exit ${String(exitCode)}`
      : null;
  const detailParts = [
    executorLabel || null,
    isRunning ? 'live' : exitLabel,
    resultSummary || null,
    elapsedLabel || null,
  ].filter(Boolean);

  return (
    <button
      type="button"
      onClick={onClick}
      className={cn(
        'w-full min-h-11 flex items-center gap-2 px-half py-1 rounded-sm text-left transition-colors',
        selected ? 'bg-tertiary' : 'hover:bg-tertiary/60',
        className
      )}
      title={`${label}: ${statusLabel}${detailParts.length ? ` - ${detailParts.join(' - ')}` : ''}`}
    >
      <IconComponent
        className="size-icon-sm flex-shrink-0 text-low"
        weight="regular"
      />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2 min-w-0">
          <span
            className={cn(
              'text-sm truncate',
              selected ? 'text-high' : 'text-normal'
            )}
          >
            {label}
          </span>
          {isRunning && <RunningDots />}
        </span>
        <span className="block text-xs text-low truncate">
          {detailParts.join(' · ')}
        </span>
      </span>
      <span
        className={cn(
          'text-[11px] leading-5 h-5 px-1.5 rounded-sm uppercase tracking-normal flex-shrink-0',
          statusColor
        )}
      >
        {statusLabel}
      </span>
    </button>
  );
}
