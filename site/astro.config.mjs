// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

import cloudflare from '@astrojs/cloudflare';

export default defineConfig({
  site: 'https://barca.sh',

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
});`,
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
            { slug: 'reference' },
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
            { slug: 'reference/cli' },
            { slug: 'reference/cache' },
            { slug: 'reference/status' },
            { slug: 'reference/logs' },
            { slug: 'reference/sql' },
            { slug: 'reference/pins' },
            { slug: 'reference/incidents' },
            { slug: 'reference/doctor' },
          ],
        },
        {
          label: 'Scheduling and Servers',
          collapsed: true,
          items: [
            { slug: 'scheduling' },
            { slug: 'deploying' },
            { slug: 'reference/server-mode' },
            { slug: 'reference/serve-ops' },
            { slug: 'reference/server-api' },
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
          label: 'Scripts and AI Agents',
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
            { slug: 'patterns' },
            { slug: 'patterns/01-asset-to-asset' },
            { slug: 'patterns/02-asset-to-task' },
            { slug: 'patterns/03-ordering-only-deps' },
            { slug: 'patterns/04-parallel-tasks' },
            { slug: 'patterns/05-conditional-execution' },
            { slug: 'patterns/06-error-handling' },
            { slug: 'patterns/08-large-inputs' },
            { slug: 'patterns/09-changed-partitions' },
            { slug: 'patterns/07-anti-patterns' },
            {
              label: 'Worked Examples',
              collapsed: true,
              items: [
                { slug: 'workflows' },
                { label: 'One Asset, No Inputs', slug: 'workflows/01-single-asset-no-inputs' },
                { label: 'One Asset, One Input', slug: 'workflows/02-single-asset-one-input' },
                { label: 'Partitions, Step by Step', slug: 'workflows/03-parametrized-assets-and-partitions' },
                { label: 'Renaming and Moving an Asset', slug: 'workflows/04-asset-continuity-rename-and-move' },
                { label: 'Freshness and Schedules', slug: 'workflows/05-schedule-driven-reconciliation-and-effects' },
                { label: 'Sensors and Outside Data', slug: 'workflows/06-sensors-and-external-observations' },
                { label: 'Timeouts and Cancelling', slug: 'workflows/09-execution-controls-and-ad-hoc-params' },
                { label: 'Tasks', slug: 'workflows/10-tasks-and-workflow-management' },
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
            { slug: 'contributing/changelog' },
            { slug: 'contributing' },
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
