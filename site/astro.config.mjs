// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

import cloudflare from '@astrojs/cloudflare';

export default defineConfig({
  site: 'https://barca.sh',

  // Old addresses of pages that moved, so shared links keep working.
  redirects: {
    '/reference/status/': '/reference/cli/status/',
    '/reference/logs/': '/reference/cli/logs/',
    '/reference/sql/': '/reference/cli/sql/',
    '/reference/doctor/': '/reference/cli/doctor/',
    '/reference/pausing/': '/reference/cli/pause/',
    '/reference/pins/': '/reference/cli/pin/',
    '/reference/cli/resume/': '/reference/cli/pause/',
    '/reference/cli/unpin/': '/reference/cli/pin/',
    '/patterns/01-asset-to-asset/': '/recipes/chain-assets/',
    '/patterns/02-asset-to-task/': '/recipes/asset-to-task/',
    '/patterns/03-ordering-only-deps/': '/recipes/ordering-only-deps/',
    '/patterns/04-parallel-tasks/': '/recipes/parallel-tasks/',
    '/patterns/05-conditional-execution/': '/recipes/conditional-execution/',
    '/patterns/06-error-handling/': '/recipes/retries-timeouts-failures/',
    '/patterns/07-anti-patterns/': '/recipes/anti-patterns/',
    '/patterns/08-large-inputs/': '/recipes/large-inputs/',
    '/patterns/09-changed-partitions/': '/recipes/changed-partitions/',
    '/workflows/01-single-asset-no-inputs/': '/examples/one-asset-no-inputs/',
    '/workflows/02-single-asset-one-input/': '/examples/one-asset-one-input/',
    '/workflows/03-parametrized-assets-and-partitions/': '/examples/partitions/',
    '/workflows/04-asset-continuity-rename-and-move/': '/examples/rename-and-move/',
    '/workflows/05-schedule-driven-reconciliation-and-effects/': '/examples/freshness-and-schedules/',
    '/workflows/06-sensors-and-external-observations/': '/examples/sensors/',
    '/workflows/09-execution-controls-and-ad-hoc-params/': '/examples/timeouts-and-cancelling/',
    '/workflows/10-tasks-and-workflow-management/': '/examples/tasks/',
    '/reference/incidents/': '/reference/cli/pause/',
    '/patterns/': '/recipes/',
    '/reference/': '/all-pages/',
    '/workflows/': '/examples/',
  },

  integrations: [
    starlight({
      title: 'Barca',
      tagline: 'Runs Python functions as a dependency graph and caches their results.',
      description:
        'Documentation for barca, which runs Python functions as a dependency graph and caches their results.',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/barca-orc/barca' },
      ],
      editLink: {
        baseUrl: 'https://github.com/barca-orc/barca/edit/main/site/',
      },
      logo: {
        light: './src/assets/mark-dark.svg',
        dark: './src/assets/mark.svg',
        alt: 'barca',
      },
      favicon: '/favicon.svg',
      expressiveCode: {
        // Long commands wrap (keeping their indent) instead of hiding trailing comments
        // off-screen; terminal output and file trees keep their columns and scroll instead.
        defaultProps: {
          wrap: true,
          hangingIndent: 4,
          overridesByLang: { ',plaintext,txt,text,ansi': { wrap: false } },
        },
        styleOverrides: {
          codeFontFamily: 'var(--sl-font-mono)',
        },
      },
      customCss: ['./src/styles/fonts.css', './src/styles/custom.css'],
      head: [
        {
          // On a fresh load Starlight shows the sidebar from the top, so a page far down the list
          // (a recipe, a worked example) was highlighted out of sight. Bring it into view once.
          tag: 'script',
          content: `document.addEventListener('DOMContentLoaded', () => {
  const pane = document.querySelector('.sidebar-pane');
  const cur = pane && pane.querySelector('a[aria-current="page"]');
  if (!cur) return;
  const r = cur.getBoundingClientRect(), p = pane.getBoundingClientRect();
  if (p.height && (r.top < p.top || r.bottom > p.bottom)) pane.scrollTop += r.top - p.top - p.height / 2;
});
// Sidebar shortcuts into a section ("Exit Codes") show as current while that section is open.
// The page's own entry then steps back, so only one entry is highlighted.
const markSection = () => {
  let inSection = false;
  document.querySelectorAll('.sidebar-content a[href*="#"]').forEach((a) => {
    const u = new URL(a.href);
    const on = u.pathname === location.pathname && u.hash === location.hash;
    a.classList.toggle('current-section', on);
    inSection ||= on;
  });
  document.querySelectorAll('.sidebar-content a[aria-current="page"]').forEach((a) =>
    a.classList.toggle('section-open', inSection));
};
document.addEventListener('DOMContentLoaded', markSection);
addEventListener('hashchange', markSection);`,
        },
      ],
      components: {
        MarkdownContent: './src/components/MarkdownContent.astro',
        PageTitle: './src/components/PageTitle.astro',
        SocialIcons: './src/components/SocialIcons.astro',
        Search: './src/components/Search.astro',
      },
      sidebar: [
        {
          label: 'Start Here',
          items: [
            { slug: 'getting-started' },
            { slug: 'guide' },
            { slug: 'team-server' },
            { slug: 'all-pages' },
          ],
        },
        {
          label: 'Why Barca',
          collapsed: true,
          items: [
            { slug: 'philosophy' },
            { slug: 'comparisons' },
            { slug: 'comparisons/framework-comparison' },
            { slug: 'comparisons/dagster' },
            { slug: 'comparisons/prefect' },
            { slug: 'comparisons/cron' },
          ],
        },
        {
          label: 'Writing Pipelines',
          collapsed: true,
          items: [
            { slug: 'reference/assets' },
            { slug: 'reference/tasks' },
            { slug: 'reference/partitions' },
            { slug: 'reference/batched-partitions' },
            { slug: 'reference/sinks' },
            { slug: 'reference/types' },
            { slug: 'reference/groups' },
            { slug: 'reference/discovery' },
            { slug: 'reference/api/decorators' },
          ],
        },
        {
          label: 'Running and Caching',
          collapsed: true,
          items: [
            { slug: 'troubleshooting' },
            { slug: 'reference/cache' },
            { slug: 'reference/refresh' },
            { slug: 'reference/external-data' },
            { slug: 'reference/hashing' },
            { slug: 'reference/artifacts' },
          ],
        },
        {
          label: 'Commands',
          collapsed: true,
          items: [
            { label: 'All Commands (CLI Reference)', slug: 'reference/cli' },
            { label: 'Exit Codes', link: '/reference/cli/#errors-and-exit-codes' },
            { slug: 'reference/cli/docs' },
            { slug: 'reference/cli/doctor' },
            { slug: 'reference/cli/gc' },
            { slug: 'reference/cli/get' },
            { slug: 'reference/cli/history' },
            { slug: 'reference/cli/list' },
            { slug: 'reference/cli/logs' },
            { slug: 'reference/cli/pause' },
            { slug: 'reference/cli/pin' },
            { slug: 'reference/cli/plan' },
            { slug: 'reference/cli/queue' },
            { slug: 'reference/cli/run' },
            { slug: 'reference/cli/serve' },
            { slug: 'reference/cli/sql' },
            { slug: 'reference/cli/state' },
            { slug: 'reference/cli/stats' },
            { slug: 'reference/cli/status' },
            { slug: 'reference/cli/tokens' },
            { slug: 'reference/cli/version' },
          ],
        },
        {
          label: 'Scheduling and Servers',
          collapsed: true,
          items: [
            { slug: 'serve' },
            { slug: 'scheduling' },
            { slug: 'web-ui' },
            { slug: 'auth' },
            { slug: 'deploying' },
            { slug: 'reference/server-mode' },
            { slug: 'reference/serve-ops' },
            { slug: 'reference/server-api' },
            { slug: 'reference/slack' },
            { slug: 'reference/telemetry' },
          ],
        },
        {
          label: 'Storage and Configuration',
          collapsed: true,
          items: [
            { slug: 'reference/config' },
            { slug: 'reference/layout' },
            { slug: 'reference/remote-storage' },
            { slug: 'reference/backup' },
            { slug: 'reference/disk' },
            { slug: 'reference/duckdb' },
          ],
        },
        {
          label: 'Automation and Agents',
          collapsed: true,
          items: [
            { slug: 'reference/agents' },
            { slug: 'reference/agent-skill' },
            { slug: 'reference/cli-contract' },
          ],
        },
        {
          label: 'Recipes',
          collapsed: true,
          items: [
            { slug: 'recipes' },
            { slug: 'recipes/chain-assets' },
            { slug: 'recipes/asset-to-task' },
            { slug: 'recipes/ordering-only-deps' },
            { slug: 'recipes/parallel-tasks' },
            { slug: 'recipes/conditional-execution' },
            { slug: 'recipes/retries-timeouts-failures' },
            { slug: 'recipes/large-inputs' },
            { slug: 'recipes/changed-partitions' },
            { slug: 'recipes/anti-patterns' },
            {
              label: 'Worked Examples',
              collapsed: true,
              items: [
                { slug: 'examples' },
                { slug: 'examples/one-asset-no-inputs' },
                { slug: 'examples/one-asset-one-input' },
                { slug: 'examples/partitions' },
                { slug: 'examples/rename-and-move' },
                { slug: 'examples/freshness-and-schedules' },
                { slug: 'examples/sensors' },
                { slug: 'examples/timeouts-and-cancelling' },
                { slug: 'examples/tasks' },
              ],
            },
          ],
        },
        {
          label: 'Internals',
          collapsed: true,
          items: [
            { slug: 'architecture' },
            { slug: 'architecture-decisions' },
            { slug: 'core-constraints' },
          ],
        },
        {
          label: 'Project',
          collapsed: true,
          items: [
            { slug: 'releases' },
            { slug: 'contributing' },
            { slug: 'contributing/changelog' },
            { slug: 'contributing/development' },
            { slug: 'contributing/testing' },
            { slug: 'contributing/releases' },
            { slug: 'rfcs' },
          ],
        },
      ],
    }),
  ],

  adapter: cloudflare(),
});
