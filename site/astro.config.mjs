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
      components: {
        MarkdownContent: './src/components/MarkdownContent.astro',
        PageTitle: './src/components/PageTitle.astro',
        SocialIcons: './src/components/SocialIcons.astro',
      },
      sidebar: [
        {
          label: 'Start Here',
          items: [
            { label: 'Getting Started', slug: 'getting-started' },
            { label: 'Guide: Beyond the First Run', slug: 'guide' },
            { label: 'Share Results With a Team', slug: 'team-server' },
            { label: 'All Reference Topics (A–Z)', slug: 'reference' },
          ],
        },
        {
          label: 'Why Barca',
          items: [
            { label: 'Philosophy', slug: 'philosophy' },
            { label: 'Compared With Other Tools', slug: 'comparisons' },
            { label: 'Framework Comparison', slug: 'comparisons/framework-comparison' },
            { label: 'vs. Dagster', slug: 'comparisons/dagster' },
            { label: 'vs. Prefect', slug: 'comparisons/prefect' },
            { label: 'vs. cron / systemd', slug: 'comparisons/cron' },
          ],
        },
        {
          label: 'Writing Pipelines',
          items: [
            { label: 'Assets, Sensors and Inputs', slug: 'reference/assets' },
            { label: 'Tasks', slug: 'reference/tasks' },
            { label: 'Partitions', slug: 'reference/partitions' },
            { label: 'Batched Partitions', slug: 'reference/batched-partitions' },
            { label: 'Sinks (Exporting Data)', slug: 'reference/sinks' },
            { label: 'Output Formats and Types', slug: 'reference/types' },
            { label: 'Groups', slug: 'reference/groups' },
            { label: 'Discovery (Which Files Count)', slug: 'reference/discovery' },
            { label: 'Decorators API', slug: 'reference/api/decorators' },
          ],
        },
        {
          label: 'Running and Caching',
          items: [
            { label: 'CLI Reference', slug: 'reference/cli' },
            { label: 'The Cache', slug: 'reference/cache' },
            { label: 'Status', slug: 'reference/status' },
            { label: 'Logs', slug: 'reference/logs' },
            { label: 'Querying Results With SQL', slug: 'reference/sql' },
            { label: 'Pinning and Rollback', slug: 'reference/pins' },
            { label: 'Pausing Assets', slug: 'reference/incidents' },
            { label: 'Doctor and Local Debugging', slug: 'reference/doctor' },
          ],
        },
        {
          label: 'Scheduling and Servers',
          items: [
            { label: 'Scheduling', slug: 'scheduling' },
            { label: 'Deploying barca serve', slug: 'deploying' },
            { label: 'Server Mode', slug: 'reference/server-mode' },
            { label: 'Serve Operations', slug: 'reference/serve-ops' },
            { label: 'Server API', slug: 'reference/server-api' },
            { label: 'Telemetry and Alerts', slug: 'reference/telemetry' },
          ],
        },
        {
          label: 'Storage and Configuration',
          items: [
            { label: 'Configuration', slug: 'reference/config' },
            { label: 'Where Things Live', slug: 'reference/layout' },
            { label: 'Remote Storage', slug: 'reference/remote-storage' },
            { label: 'Metadata Backups', slug: 'reference/backup' },
            { label: 'Disk Use', slug: 'reference/disk' },
            { label: 'DuckDB Versions', slug: 'reference/duckdb' },
          ],
        },
        {
          label: 'Scripts and AI Agents',
          items: [
            { label: 'Scripts and AI Agents', slug: 'reference/agents' },
            { label: 'Agent Skill', slug: 'reference/agent-skill' },
            { label: 'CLI Contract', slug: 'reference/cli-contract' },
          ],
        },
        {
          label: 'Recipes',
          items: [
            { label: 'All Recipes', slug: 'patterns' },
            { label: 'Chain Assets', slug: 'patterns/01-asset-to-asset' },
            { label: 'Feed an Asset to a Task', slug: 'patterns/02-asset-to-task' },
            { label: 'Run After a Step, Without Its Value', slug: 'patterns/03-ordering-only-deps' },
            { label: 'Run Tasks in Parallel', slug: 'patterns/04-parallel-tasks' },
            { label: 'Branch or Stop Conditionally', slug: 'patterns/05-conditional-execution' },
            { label: 'Retries, Timeouts and Failures', slug: 'patterns/06-error-handling' },
            { label: 'Read Large Inputs Lazily', slug: 'patterns/08-large-inputs' },
            { label: 'Rerun Only Changed Partitions', slug: 'patterns/09-changed-partitions' },
            { label: 'Anti-Patterns to Avoid', slug: 'patterns/07-anti-patterns' },
            {
              label: 'Worked Examples',
              collapsed: true,
              items: [
                { label: 'All Worked Examples', slug: 'workflows' },
                { label: 'One Asset, No Inputs', slug: 'workflows/01-single-asset-no-inputs' },
                { label: 'One Asset, One Input', slug: 'workflows/02-single-asset-one-input' },
                {
                  label: 'Parametrized Assets and Partitions',
                  slug: 'workflows/03-parametrized-assets-and-partitions',
                },
                {
                  label: 'Renaming and Moving an Asset',
                  slug: 'workflows/04-asset-continuity-rename-and-move',
                },
                {
                  label: 'Freshness and Schedules',
                  slug: 'workflows/05-schedule-driven-reconciliation-and-effects',
                },
                {
                  label: 'Sensors and Outside Data',
                  slug: 'workflows/06-sensors-and-external-observations',
                },
                {
                  label: 'Timeouts, Retries and Cancelling',
                  slug: 'workflows/09-execution-controls-and-ad-hoc-params',
                },
                {
                  label: 'Tasks and Workflow Management',
                  slug: 'workflows/10-tasks-and-workflow-management',
                },
              ],
            },
          ],
        },
        {
          label: 'Internals',
          collapsed: true,
          items: [
            { label: 'Architecture', slug: 'architecture' },
            { label: 'Architecture Decisions', slug: 'architecture-decisions' },
            { label: 'Core Constraints', slug: 'core-constraints' },
          ],
        },
        {
          label: 'Project',
          collapsed: true,
          items: [
            { label: 'Changelog', slug: 'contributing/changelog' },
            { label: 'Contributing', slug: 'contributing' },
            { label: 'Development Setup', slug: 'contributing/development' },
            { label: 'Testing', slug: 'contributing/testing' },
            { label: 'Releases', slug: 'contributing/releases' },
            {
              label: 'Design Records (RFCs)',
              collapsed: true,
              items: [
                { label: 'All RFCs', slug: 'rfcs' },
                { label: 'Process & Template', slug: 'rfcs/template' },
                { label: 'RFC-0001: Node Kinds & Freshness', slug: 'rfcs/0001-node-kinds-and-freshness' },
                { label: 'RFC-0002: CLI Surface', slug: 'rfcs/0002-cli-surface' },
                { label: 'RFC-0003: Decorators & Python API', slug: 'rfcs/0003-decorator-and-python-api' },
                { label: 'RFC-0004: HTTP Server API', slug: 'rfcs/0004-http-server-api' },
                {
                  label: 'RFC-0005: Artifacts & Storage',
                  slug: 'rfcs/0005-artifact-serialization-and-storage',
                },
                {
                  label: 'RFC-0006: Config & Remote State',
                  slug: 'rfcs/0006-configuration-and-remote-state',
                },
                { label: 'RFC-0007: Server Mode', slug: 'rfcs/0007-server-mode' },
                { label: 'RFC-0008: Freshness Propagation', slug: 'rfcs/0008-freshness-propagation' },
                { label: 'RFC-0009: Store Backup & Restore', slug: 'rfcs/0009-store-backup-and-restore' },
                { label: 'RFC-0010: Slack Notifications', slug: 'rfcs/0010-slack-notifications' },
                { label: 'RFC-0011: Signed URLs', slug: 'rfcs/0011-signed-urls' },
                { label: 'RFC-0012: A Size Limit for .barca', slug: 'rfcs/0012-cache-size-limit' },
                { label: 'RFC-0013: Batched Partitions', slug: 'rfcs/0013-batched-partitions' },
                { label: 'RFC-0015: Metadata Backup Cadence', slug: 'rfcs/0015-backup-cadence' },
                { label: 'RFC-0016: Tokens & Stream Resume', slug: 'rfcs/0016-server-tokens-and-stream-resume' },
              ],
            },
          ],
        },
      ],
    }),
  ],

  adapter: cloudflare(),
});
