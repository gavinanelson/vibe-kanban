import {
  useState,
  useEffect,
  useRef,
  useCallback,
  type ReactNode,
  type RefObject,
} from 'react';
import { useTranslation } from 'react-i18next';
import type { LocalAttachmentMetadata } from './WorkspaceContext';
import { cn } from '../lib/cn';
import {
  XIcon,
  LinkIcon,
  DotsThreeIcon,
  TrashIcon,
  PaperclipIcon,
  ImageIcon,
  ArrowSquareOutIcon,
  ArrowsClockwiseIcon,
  GithubLogoIcon,
  RobotIcon,
  PlayIcon,
  CheckCircleIcon,
  CircleIcon,
  ClockIcon,
  WarningCircleIcon,
} from '@phosphor-icons/react';
import {
  IssueTagsRow,
  type IssueTagBase,
  type IssueTagsRowAddTagControlProps,
  type LinkedPullRequest as IssueTagsLinkedPullRequest,
} from './IssueTagsRow';
import { PrimaryButton } from './PrimaryButton';
import { Toggle } from './Toggle';
import {
  IssuePropertyRow,
  type IssuePropertyRowProps,
} from './IssuePropertyRow';
import { IconButton } from './IconButton';
import { AutoResizeTextarea } from './AutoResizeTextarea';
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from './RadixTooltip';
import { ErrorAlert } from './ErrorAlert';

export type IssuePanelMode = 'create' | 'edit';
type IssuePriority = IssuePropertyRowProps['priority'];
type IssueStatus = IssuePropertyRowProps['statuses'][number];
type IssueAssignee = NonNullable<
  IssuePropertyRowProps['assigneeUsers']
>[number];
type IssueCreator = Exclude<IssuePropertyRowProps['creatorUser'], undefined>;
export interface KanbanIssueTag extends IssueTagBase {
  project_id: string;
}

export interface IssueFormData {
  title: string;
  description: string | null;
  statusId: string;
  priority: IssuePriority | null;
  assigneeIds: string[];
  tagIds: string[];
  createDraftWorkspace: boolean;
}

export interface LinkedPullRequest extends IssueTagsLinkedPullRequest {}

export interface ImplicationAutopilotPanelStatus {
  implementationState: string;
  autoReviewState: string;
  latestReviewDecision: string;
  latestReviewExcerpt?: string | null;
  reviewFixState: string;
  prMergeState: string;
  nextAction: string;
  nextActionLabel: string;
  nextActionDescription: string;
  currentStepLabel: string;
  blocker?: string | null;
  steps: {
    label: string;
    state: 'completed' | 'running' | 'blocked' | 'available' | 'not_started';
    summary: string;
    sessionName?: string | null;
    processId?: string | null;
    processStatus?: string | null;
  }[];
  tokenSafetyState: 'idle' | 'guarded' | 'blocked';
  tokenSafetyNote: string;
  defaultModel: string;
  defaultReasoning: string;
  daemonized: boolean;
}

export interface LinkedGitHubIssue {
  repoFullName: string;
  issueNumber: number;
  issueUrl: string;
  state?: string | null;
  latestPrNumber?: number | null;
  latestPrUrl?: string | null;
  latestAgentCheckpoint?: string | null;
}

export interface KanbanIssueDescriptionEditorProps {
  placeholder: string;
  value: string;
  onChange: (value: string) => void;
  onCmdEnter?: () => void;
  onPasteFiles?: (files: File[]) => void;
  disabled?: boolean;
  autoFocus?: boolean;
  className?: string;
  localAttachments?: LocalAttachmentMetadata[];
  showStaticToolbar?: boolean;
  saveStatus?: 'idle' | 'saved';
  staticToolbarActions?: ReactNode;
  onRequestEdit?: () => void;
  hideActions?: boolean;
}

export interface KanbanIssuePanelProps {
  mode: IssuePanelMode;
  displayId: string;

  // Form data
  formData: IssueFormData;
  onFormChange: <K extends keyof IssueFormData>(
    field: K,
    value: IssueFormData[K]
  ) => void;

  // Options for dropdowns
  statuses: IssueStatus[];
  tags: KanbanIssueTag[];

  // Resolved assignee profiles for avatar display
  assigneeUsers?: IssueAssignee[];

  // Edit mode data
  issueId?: string | null;
  creatorUser?: IssueCreator;
  parentIssue?: { id: string; simpleId: string } | null;
  onParentIssueClick?: () => void;
  onRemoveParentIssue?: () => void;
  linkedPrs?: LinkedPullRequest[];
  onLinkPr?: () => void;
  linkedGitHubIssue?: LinkedGitHubIssue | null;
  onManageGitHubIssueLink?: () => void;
  onRefreshGitHubIssue?: () => void;
  isRefreshingGitHubIssue?: boolean;
  implicationAutopilotStatus?: ImplicationAutopilotPanelStatus | null;
  isImplicationAutopilotLoading?: boolean;
  onRefreshImplicationAutopilot?: () => void;
  onStartImplicationAutoReview?: () => void;
  isStartingImplicationAutoReview?: boolean;
  onStartImplicationReviewFix?: () => void;
  isStartingImplicationReviewFix?: boolean;
  onOpenImplicationMergeHandoff?: () => void;

  // Actions
  onClose: () => void;
  onSubmit: () => void;
  onCmdEnterSubmit?: () => void;
  onDeleteDraft?: () => void;

  // Tag create callback - returns the new tag ID
  onCreateTag?: (data: { name: string; color: string }) => string;
  renderAddTagControl?: (
    props: IssueTagsRowAddTagControlProps<KanbanIssueTag>
  ) => ReactNode;
  renderDescriptionEditor: (
    props: KanbanIssueDescriptionEditorProps
  ) => ReactNode;

  // Loading states
  isSubmitting?: boolean;

  // Save status for description field
  descriptionSaveStatus?: 'idle' | 'saved';

  // Ref for title input (created in container)
  titleInputRef: RefObject<HTMLTextAreaElement>;

  // Copy link callback (edit mode only)
  onCopyLink?: () => void;

  // More actions callback (edit mode only) - opens command bar with issue actions
  onMoreActions?: () => void;

  // Image attachment upload
  onPasteFiles?: (files: File[]) => void;
  localAttachments?: LocalAttachmentMetadata[];
  dropzoneProps?: {
    getRootProps: () => Record<string, unknown>;
    getInputProps: () => Record<string, unknown>;
    isDragActive: boolean;
  };
  onBrowseAttachment?: () => void;
  isUploading?: boolean;
  attachmentError?: string | null;
  onDismissAttachmentError?: () => void;

  // Edit-mode section renderers
  renderWorkspacesSection?: (issueId: string) => ReactNode;
  renderRelationshipsSection?: (issueId: string) => ReactNode;
  renderSubIssuesSection?: (issueId: string) => ReactNode;
  renderCommentsSection?: (issueId: string) => ReactNode;
}

const AUTOPILOT_STEP_STYLE = {
  completed: {
    icon: CheckCircleIcon,
    className: 'border-success/40 bg-success/5 text-success',
    label: 'Completed',
  },
  running: {
    icon: ClockIcon,
    className: 'border-brand/50 bg-brand/10 text-brand',
    label: 'Running',
  },
  blocked: {
    icon: WarningCircleIcon,
    className: 'border-warning/50 bg-warning/10 text-warning',
    label: 'Blocked',
  },
  available: {
    icon: PlayIcon,
    className: 'border-brand/50 bg-brand/10 text-brand',
    label: 'Current',
  },
  not_started: {
    icon: CircleIcon,
    className: 'border-border bg-panel text-low',
    label: 'Not started',
  },
} as const;

function AutopilotStatePill({
  state,
}: {
  state: keyof typeof AUTOPILOT_STEP_STYLE;
}) {
  const style = AUTOPILOT_STEP_STYLE[state];

  return (
    <span
      className={cn(
        'inline-flex items-center rounded-sm border px-half py-0.5 text-[10px] font-medium uppercase tracking-normal',
        style.className
      )}
    >
      {style.label}
    </span>
  );
}

function AutopilotStepRow({
  step,
  isCurrent,
}: {
  step: ImplicationAutopilotPanelStatus['steps'][number];
  isCurrent: boolean;
}) {
  const style = AUTOPILOT_STEP_STYLE[step.state];
  const Icon = style.icon;

  return (
    <div
      className={cn(
        'grid grid-cols-[1.25rem_1fr] gap-half rounded-sm px-half py-half',
        isCurrent && 'bg-panel/70'
      )}
    >
      <div className="pt-0.5">
        <Icon
          className={cn(
            'size-icon-sm',
            step.state === 'running' && 'animate-pulse'
          )}
          weight={step.state === 'not_started' ? 'regular' : 'fill'}
        />
      </div>
      <div className="min-w-0">
        <div className="flex min-w-0 flex-wrap items-center gap-half">
          <span className="text-xs font-medium text-high">{step.label}</span>
          <AutopilotStatePill state={step.state} />
          {isCurrent && (
            <span className="text-[10px] uppercase tracking-normal text-low">
              Current step
            </span>
          )}
        </div>
        <p className="mt-0.5 text-xs text-low">{step.summary}</p>
        {(step.sessionName || step.processId || step.processStatus) && (
          <p className="mt-0.5 truncate font-ibm-plex-mono text-[11px] text-low">
            {step.sessionName ?? 'Session'} · {step.processStatus ?? 'unknown'}
            {step.processId ? ` · ${step.processId.slice(0, 8)}` : ''}
          </p>
        )}
      </div>
    </div>
  );
}

export function KanbanIssuePanel({
  mode,
  displayId,
  formData,
  onFormChange,
  statuses,
  tags,
  assigneeUsers,
  issueId,
  creatorUser,
  parentIssue,
  onParentIssueClick,
  onRemoveParentIssue,
  linkedPrs = [],
  onLinkPr,
  linkedGitHubIssue,
  onManageGitHubIssueLink,
  onRefreshGitHubIssue,
  isRefreshingGitHubIssue,
  implicationAutopilotStatus,
  isImplicationAutopilotLoading,
  onRefreshImplicationAutopilot,
  onStartImplicationAutoReview,
  isStartingImplicationAutoReview,
  onStartImplicationReviewFix,
  isStartingImplicationReviewFix,
  onOpenImplicationMergeHandoff,
  onClose,
  onSubmit,
  onCmdEnterSubmit,
  onDeleteDraft,
  onCreateTag,
  renderAddTagControl,
  renderDescriptionEditor,
  isSubmitting,
  descriptionSaveStatus,
  titleInputRef,
  onCopyLink,
  onMoreActions,
  onPasteFiles,
  localAttachments,
  dropzoneProps,
  onBrowseAttachment,
  isUploading,
  attachmentError,
  onDismissAttachmentError,
  renderWorkspacesSection,
  renderRelationshipsSection,
  renderSubIssuesSection,
  renderCommentsSection,
}: KanbanIssuePanelProps) {
  const { t } = useTranslation('common');
  const isCreateMode = mode === 'create';
  const breadcrumbTextClass =
    'min-w-0 text-sm text-normal truncate rounded-sm px-1 py-0.5 hover:bg-panel hover:text-high transition-colors';
  const creatorName =
    creatorUser?.first_name?.trim() || creatorUser?.username?.trim() || null;
  const showCreator = !isCreateMode && Boolean(creatorName);

  // Description edit state: in edit mode, show preview by default; in create mode, always editable
  const [isDescriptionEditing, setIsDescriptionEditing] =
    useState(isCreateMode);
  const descriptionContainerRef = useRef<HTMLDivElement>(null);

  // Reset description editing state when switching between create/edit mode or when issue changes
  useEffect(() => {
    setIsDescriptionEditing(isCreateMode);
  }, [isCreateMode, issueId]);

  // Click outside the description area to exit editing
  const handleDescriptionBlur = useCallback(() => {
    if (!isCreateMode) {
      setIsDescriptionEditing(false);
    }
  }, [isCreateMode]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      const target = e.target as HTMLElement;
      const isEditable =
        target.tagName === 'INPUT' ||
        target.tagName === 'TEXTAREA' ||
        target.isContentEditable;
      if (isEditable) {
        // If editing description, exit edit mode first
        if (
          isDescriptionEditing &&
          !isCreateMode &&
          descriptionContainerRef.current?.contains(target)
        ) {
          setIsDescriptionEditing(false);
        }
        target.blur();
        (e.currentTarget as HTMLElement).focus();
        e.stopPropagation();
      } else {
        onClose();
      }
    }
  };

  const handleTitleKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if ((e.metaKey || e.ctrlKey) && e.key === 'Enter') {
      e.preventDefault();
      onCmdEnterSubmit?.();
    }
  };

  return (
    <div
      className="flex flex-col h-full overflow-hidden outline-none"
      onKeyDown={handleKeyDown}
      tabIndex={-1}
    >
      {/* Header */}
      <div className="flex items-center justify-between px-base py-half border-b shrink-0">
        <div className="flex items-center gap-half min-w-0 font-ibm-plex-mono">
          <span className={`${breadcrumbTextClass} shrink-0`}>{displayId}</span>
          {!isCreateMode && onCopyLink && (
            <button
              type="button"
              onClick={onCopyLink}
              className="p-half rounded-sm text-low hover:text-normal hover:bg-panel transition-colors"
              aria-label={t('kanban.copyLink')}
            >
              <LinkIcon className="size-icon-sm" weight="bold" />
            </button>
          )}
        </div>
        <div className="flex items-center gap-half">
          {!isCreateMode && onMoreActions && (
            <button
              type="button"
              onClick={onMoreActions}
              className="p-half rounded-sm text-low hover:text-normal hover:bg-panel transition-colors"
              aria-label={t('kanban.moreActions')}
            >
              <DotsThreeIcon className="size-icon-sm" weight="bold" />
            </button>
          )}
          <button
            type="button"
            onClick={onClose}
            className="p-half rounded-sm text-low hover:text-normal hover:bg-panel transition-colors"
            aria-label={t('kanban.closePanel')}
          >
            <XIcon className="size-icon-sm" weight="bold" />
          </button>
        </div>
      </div>

      {/* Scrollable Content */}
      <div className="flex-1 overflow-y-auto">
        {/* Property Row */}
        <div className="px-base py-base border-b">
          <IssuePropertyRow
            statusId={formData.statusId}
            priority={formData.priority}
            assigneeIds={formData.assigneeIds}
            assigneeUsers={assigneeUsers}
            statuses={statuses}
            creatorUser={showCreator ? creatorUser : undefined}
            parentIssue={parentIssue}
            onParentIssueClick={onParentIssueClick}
            onRemoveParentIssue={onRemoveParentIssue}
            onStatusClick={() => onFormChange('statusId', formData.statusId)}
            onPriorityClick={() => onFormChange('priority', formData.priority)}
            onAssigneeClick={() =>
              onFormChange('assigneeIds', formData.assigneeIds)
            }
            disabled={isSubmitting}
          />
        </div>

        {/* Tags Row */}
        <div className="px-base py-base border-b">
          <IssueTagsRow
            selectedTagIds={formData.tagIds}
            availableTags={tags}
            linkedPrs={isCreateMode ? [] : linkedPrs}
            onTagsChange={(tagIds) => onFormChange('tagIds', tagIds)}
            onCreateTag={onCreateTag}
            renderAddTagControl={renderAddTagControl}
            onLinkPr={!isCreateMode ? onLinkPr : undefined}
            disabled={isSubmitting}
          />
        </div>

        {(linkedGitHubIssue || onManageGitHubIssueLink) && (
          <div className="px-base py-base border-b">
            {linkedGitHubIssue ? (
              <div className="rounded-md border border-border bg-muted/30 px-base py-half">
                <div className="flex items-start justify-between gap-base">
                  <div className="min-w-0">
                    <div className="flex items-center gap-half text-sm font-medium text-high">
                      <GithubLogoIcon className="size-icon-sm" weight="fill" />
                      <span className="truncate">
                        GH #{linkedGitHubIssue.issueNumber}
                      </span>
                      {linkedGitHubIssue.state && (
                        <span className="text-low font-normal lowercase">
                          {linkedGitHubIssue.state}
                        </span>
                      )}
                    </div>
                    <div className="mt-half text-xs text-low truncate">
                      {linkedGitHubIssue.repoFullName}
                    </div>
                    {linkedGitHubIssue.latestPrNumber && (
                      <div className="mt-half text-xs text-low truncate">
                        PR #{linkedGitHubIssue.latestPrNumber}
                      </div>
                    )}
                    {linkedGitHubIssue.latestAgentCheckpoint && (
                      <div className="mt-half text-xs text-low">
                        {linkedGitHubIssue.latestAgentCheckpoint}
                      </div>
                    )}
                  </div>
                  <div className="flex items-center gap-half shrink-0">
                    {onRefreshGitHubIssue && (
                      <button
                        type="button"
                        onClick={onRefreshGitHubIssue}
                        disabled={isRefreshingGitHubIssue}
                        className="p-half rounded-sm text-low hover:text-normal hover:bg-panel transition-colors disabled:opacity-50"
                        aria-label="Refresh GitHub issue"
                        title="Refresh GitHub issue"
                      >
                        <ArrowsClockwiseIcon
                          className={cn(
                            'size-icon-sm',
                            isRefreshingGitHubIssue && 'animate-spin'
                          )}
                          weight="bold"
                        />
                      </button>
                    )}
                    {onManageGitHubIssueLink && (
                      <button
                        type="button"
                        onClick={onManageGitHubIssueLink}
                        className="p-half rounded-sm text-low hover:text-normal hover:bg-panel transition-colors"
                        aria-label="Manage GitHub link"
                        title="Manage GitHub link"
                      >
                        <LinkIcon className="size-icon-sm" weight="bold" />
                      </button>
                    )}
                    <a
                      href={linkedGitHubIssue.issueUrl}
                      target="_blank"
                      rel="noopener noreferrer"
                      className="p-half rounded-sm text-low hover:text-normal hover:bg-panel transition-colors"
                      aria-label="Open linked GitHub issue"
                      title="Open linked GitHub issue"
                    >
                      <ArrowSquareOutIcon
                        className="size-icon-sm"
                        weight="bold"
                      />
                    </a>
                  </div>
                </div>
              </div>
            ) : (
              <button
                type="button"
                onClick={onManageGitHubIssueLink}
                className="flex items-center gap-half rounded-sm border border-dashed border-border px-half py-half text-sm text-low hover:text-normal hover:bg-panel transition-colors"
              >
                <GithubLogoIcon className="size-icon-sm" weight="fill" />
                Link GitHub issue
              </button>
            )}
          </div>
        )}

        {(implicationAutopilotStatus || isImplicationAutopilotLoading) && (
          <div className="px-base py-base border-b">
            <div className="rounded-md border border-border bg-muted/20 px-base py-base">
              <div className="flex flex-col gap-base">
                <div className="flex items-start justify-between gap-base">
                  <div className="min-w-0">
                    <div className="flex flex-wrap items-center gap-half text-sm font-medium text-high">
                      <RobotIcon className="size-icon-sm" weight="bold" />
                      <span>Implication autopilot</span>
                      {implicationAutopilotStatus?.currentStepLabel && (
                        <span className="rounded-sm bg-panel px-half py-0.5 text-[10px] uppercase tracking-normal text-low">
                          {implicationAutopilotStatus.currentStepLabel}
                        </span>
                      )}
                      {implicationAutopilotStatus?.daemonized === false && (
                        <span className="rounded-sm bg-panel px-half py-0.5 text-[10px] uppercase tracking-normal text-low">
                          status slice
                        </span>
                      )}
                    </div>
                    <p className="mt-half text-xs text-low">
                      {implicationAutopilotStatus?.nextActionDescription ??
                        'Loading autopilot status.'}
                    </p>
                  </div>
                  <div className="flex shrink-0 flex-wrap justify-end gap-half">
                    {onRefreshImplicationAutopilot && (
                      <button
                        type="button"
                        onClick={onRefreshImplicationAutopilot}
                        className="p-half rounded-sm text-low hover:text-normal hover:bg-panel transition-colors"
                        aria-label="Refresh autopilot status"
                        title="Refresh autopilot status"
                      >
                        <ArrowsClockwiseIcon
                          className={cn(
                            'size-icon-sm',
                            isImplicationAutopilotLoading && 'animate-spin'
                          )}
                          weight="bold"
                        />
                      </button>
                    )}
                    {onStartImplicationAutoReview &&
                      implicationAutopilotStatus?.nextAction ===
                        'start_auto_review' && (
                        <button
                          type="button"
                          onClick={onStartImplicationAutoReview}
                          disabled={isStartingImplicationAutoReview}
                          className="inline-flex items-center gap-half rounded-sm bg-brand px-half py-half text-xs text-on-brand hover:bg-brand-hover disabled:opacity-50"
                        >
                          <PlayIcon className="size-icon-xs" weight="bold" />
                          {implicationAutopilotStatus.currentStepLabel ===
                          'Re-review'
                            ? 'Start re-review'
                            : 'Start review'}
                        </button>
                      )}
                    {onStartImplicationReviewFix &&
                      implicationAutopilotStatus?.nextAction ===
                        'start_review_fix' && (
                        <button
                          type="button"
                          onClick={onStartImplicationReviewFix}
                          disabled={isStartingImplicationReviewFix}
                          className="inline-flex items-center gap-half rounded-sm bg-brand px-half py-half text-xs text-on-brand hover:bg-brand-hover disabled:opacity-50"
                        >
                          <PlayIcon className="size-icon-xs" weight="bold" />
                          Start review fix
                        </button>
                      )}
                    {implicationAutopilotStatus?.nextAction ===
                      'ready_for_merge' && (
                      <button
                        type="button"
                        onClick={onOpenImplicationMergeHandoff}
                        disabled={!onOpenImplicationMergeHandoff}
                        className="inline-flex items-center gap-half rounded-sm border border-success px-half py-half text-xs text-success hover:bg-success/10 disabled:border-border disabled:text-low disabled:opacity-70"
                        title={
                          onOpenImplicationMergeHandoff
                            ? 'Open the linked PR for manual merge handoff.'
                            : 'Review passed. Open the workspace Git controls or linked PR to merge manually.'
                        }
                      >
                        <ArrowSquareOutIcon
                          className="size-icon-xs"
                          weight="bold"
                        />
                        Open merge handoff
                      </button>
                    )}
                  </div>
                </div>

                {implicationAutopilotStatus ? (
                  <>
                    <div className="space-y-0.5">
                      {implicationAutopilotStatus.steps.map((step) => (
                        <AutopilotStepRow
                          key={step.label}
                          step={step}
                          isCurrent={
                            step.label ===
                            implicationAutopilotStatus.currentStepLabel
                          }
                        />
                      ))}
                    </div>

                    <div className="rounded-sm border border-border bg-panel/50 px-half py-half">
                      <div className="flex flex-wrap items-center gap-half text-xs">
                        <span className="font-medium text-high">
                          Review decision
                        </span>
                        <span className="text-low">
                          {implicationAutopilotStatus.latestReviewDecision}
                        </span>
                      </div>
                      {implicationAutopilotStatus.latestReviewExcerpt && (
                        <p className="mt-half line-clamp-3 text-xs text-low">
                          {implicationAutopilotStatus.latestReviewExcerpt}
                        </p>
                      )}
                      {implicationAutopilotStatus.blocker && (
                        <p className="mt-half text-xs text-warning">
                          {implicationAutopilotStatus.blocker}
                        </p>
                      )}
                    </div>

                    <div
                      className={cn(
                        'rounded-sm border px-half py-half text-xs',
                        implicationAutopilotStatus.tokenSafetyState ===
                          'blocked'
                          ? 'border-warning/50 bg-warning/10 text-warning'
                          : 'border-border bg-panel/50 text-low'
                      )}
                    >
                      <span className="font-medium text-high">
                        Token safety:{' '}
                      </span>
                      {implicationAutopilotStatus.tokenSafetyNote}
                      <span className="block pt-0.5 text-[11px] text-low">
                        Uses {implicationAutopilotStatus.defaultModel},{' '}
                        {implicationAutopilotStatus.defaultReasoning} reasoning.
                        Completed sessions with unseen output are idle; only
                        running sessions are spending tokens.
                      </span>
                    </div>
                  </>
                ) : (
                  <p className="text-xs text-low">Loading status timeline.</p>
                )}
              </div>
            </div>
          </div>
        )}

        {/* Title and Description */}
        <div className="rounded-sm">
          {/* Title Input */}
          <div className="w-full mt-base">
            <AutoResizeTextarea
              ref={titleInputRef}
              value={formData.title}
              onChange={(value) => onFormChange('title', value)}
              onKeyDown={handleTitleKeyDown}
              placeholder="Issue Title..."
              autoFocus={isCreateMode}
              aria-label="Issue title"
              disabled={isSubmitting}
              className={cn(
                'px-base text-lg font-medium text-high',
                'placeholder:text-high/50',
                isSubmitting && 'opacity-50 pointer-events-none'
              )}
            />

            <div
              className={cn(
                'pointer-events-none absolute inset-0 px-base',
                'text-high/50 font-medium text-lg',
                'hidden',
                "[[data-empty='true']_+_&]:block" // show placeholder when previous sibling data-empty=true
              )}
            >
              {t('kanban.issueTitlePlaceholder')}
            </div>
          </div>

          {/* Description WYSIWYG Editor with image dropzone */}
          <div
            ref={descriptionContainerRef}
            {...(isDescriptionEditing ? dropzoneProps?.getRootProps() : {})}
            className={cn(
              'relative mt-base',
              !isDescriptionEditing && !isCreateMode && 'cursor-text'
            )}
            onClick={() => {
              if (!isDescriptionEditing && !isCreateMode && !isSubmitting) {
                // Don't enter edit mode if the user was selecting text
                const selection = window.getSelection();
                if (selection && selection.toString().length > 0) return;
                setIsDescriptionEditing(true);
              }
            }}
            onBlur={(e) => {
              // Exit edit mode when focus leaves the description container
              if (
                descriptionContainerRef.current &&
                !descriptionContainerRef.current.contains(
                  e.relatedTarget as Node
                )
              ) {
                handleDescriptionBlur();
              }
            }}
          >
            {isDescriptionEditing && (
              <input
                {...(dropzoneProps?.getInputProps() as React.InputHTMLAttributes<HTMLInputElement>)}
                data-dropzone-input
              />
            )}
            {renderDescriptionEditor({
              placeholder: isDescriptionEditing
                ? t('kanban.issueDescriptionPlaceholder')
                : formData.description
                  ? ''
                  : t('kanban.issueDescriptionPlaceholder'),
              value: formData.description ?? '',
              onChange: (value) => onFormChange('description', value || null),
              onCmdEnter: onCmdEnterSubmit,
              onPasteFiles: isDescriptionEditing ? onPasteFiles : undefined,
              disabled: !isDescriptionEditing || isSubmitting,
              autoFocus: false,
              className: cn(
                'px-base',
                isDescriptionEditing ? 'min-h-[100px]' : 'min-h-[2rem]',
                !isDescriptionEditing && !formData.description && 'text-low'
              ),
              localAttachments,
              showStaticToolbar: !isCreateMode || isDescriptionEditing,
              hideActions: true,
              saveStatus: descriptionSaveStatus,
              onRequestEdit: !isCreateMode
                ? () => setIsDescriptionEditing(true)
                : undefined,
              staticToolbarActions: (
                <>
                  {isDescriptionEditing && onBrowseAttachment && (
                    <TooltipProvider>
                      <Tooltip>
                        <TooltipTrigger asChild>
                          <button
                            type="button"
                            onMouseDown={(e) => {
                              e.preventDefault();
                              if (!isSubmitting && !isUploading) {
                                onBrowseAttachment();
                              }
                            }}
                            disabled={isSubmitting || isUploading}
                            className={cn(
                              'p-half rounded-sm transition-colors',
                              'text-low hover:text-normal hover:bg-panel/50',
                              'disabled:opacity-50 disabled:cursor-not-allowed'
                            )}
                            title={t('kanban.attachFile')}
                            aria-label={t('kanban.attachFile')}
                          >
                            <PaperclipIcon className="size-icon-sm" />
                          </button>
                        </TooltipTrigger>
                        <TooltipContent>
                          {t('kanban.attachFileHint')}
                        </TooltipContent>
                      </Tooltip>
                    </TooltipProvider>
                  )}
                </>
              ),
            })}
            {attachmentError && (
              <div className="px-base">
                <ErrorAlert
                  message={attachmentError}
                  className="mt-half mb-half"
                  onDismiss={onDismissAttachmentError}
                  dismissLabel={t('buttons.close')}
                />
              </div>
            )}
            {dropzoneProps?.isDragActive && (
              <div className="absolute inset-0 z-50 bg-primary/80 backdrop-blur-sm border-2 border-dashed border-brand rounded flex items-center justify-center pointer-events-none animate-in fade-in-0 duration-150">
                <div className="text-center">
                  <div className="mx-auto mb-2 w-10 h-10 rounded-full bg-brand/10 flex items-center justify-center">
                    <ImageIcon className="h-5 w-5 text-brand" />
                  </div>
                  <p className="text-sm font-medium text-high">
                    {t('kanban.dropFilesHere')}
                  </p>
                  <p className="text-xs text-low mt-0.5">
                    {t('kanban.fileDropHint')}
                  </p>
                </div>
              </div>
            )}
          </div>
        </div>

        {/* Create Draft Workspace Toggle (Create mode only) */}
        {isCreateMode && (
          <div className="p-base border-t">
            <Toggle
              checked={formData.createDraftWorkspace}
              onCheckedChange={(checked) =>
                onFormChange('createDraftWorkspace', checked)
              }
              label={t('kanban.createDraftWorkspaceImmediately')}
              description={t('kanban.createDraftWorkspaceDescription')}
              disabled={isSubmitting}
            />
          </div>
        )}

        {/* Create Issue Button (Create mode only) */}
        {isCreateMode && (
          <div className="px-base pb-base flex items-center gap-half">
            <PrimaryButton
              value={t('kanban.createIssue')}
              onClick={onSubmit}
              disabled={isSubmitting || isUploading || !formData.title.trim()}
              actionIcon={isSubmitting ? 'spinner' : undefined}
              variant="default"
            />
            {onDeleteDraft && (
              <IconButton
                icon={TrashIcon}
                onClick={onDeleteDraft}
                disabled={isSubmitting}
                aria-label="Delete draft"
                title="Delete draft"
                className="hover:text-error hover:bg-error/10"
              />
            )}
          </div>
        )}

        {/* Workspaces Section (Edit mode only) */}
        {!isCreateMode && issueId && renderWorkspacesSection && (
          <div className="border-t">{renderWorkspacesSection(issueId)}</div>
        )}

        {/* Relationships Section (Edit mode only) */}
        {!isCreateMode && issueId && renderRelationshipsSection && (
          <div className="border-t">{renderRelationshipsSection(issueId)}</div>
        )}

        {/* Sub-Issues Section (Edit mode only) */}
        {!isCreateMode && issueId && renderSubIssuesSection && (
          <div className="border-t">{renderSubIssuesSection(issueId)}</div>
        )}

        {/* Comments Section (Edit mode only) */}
        {!isCreateMode && issueId && renderCommentsSection && (
          <div className="border-t">{renderCommentsSection(issueId)}</div>
        )}
      </div>
    </div>
  );
}
