const PROJECT_REPO_DEFAULTS: Record<string, string> = {
  implication: 'gavinanelson/implication',
};

export const FALLBACK_GITHUB_REPO = 'gavinanelson/implication';

export function normalizeProjectNameForRepoDefault(name: string): string {
  return name.trim().toLowerCase();
}

export function getProjectDefaultGitHubRepo(
  projectName: string | null | undefined
): string | null {
  if (!projectName) {
    return null;
  }

  return (
    PROJECT_REPO_DEFAULTS[normalizeProjectNameForRepoDefault(projectName)] ??
    null
  );
}
