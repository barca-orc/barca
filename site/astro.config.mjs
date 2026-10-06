// @ts-check
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

import cloudflare from '@astrojs/cloudflare';

export default defineConfig({
  site: 'https://barca.sh',

  integrations: [
    starlight({
      title: 'Barca',
      tagline: 'The invisible asset orchestrator.',
      description:
        'Rust plans it. Python runs it. You just write functions. Docs for the barca asset orchestrator.',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/barca-orc/barca' },
      ],
      editLink: {
        baseUrl: 'https://github.com/barca-orc/barca/edit/main/site/src/content/docs/',
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
      sidebar: [
        {
          label: 'Start Here',
          items: [
            { label: 'Getting Started', slug: 'getting-started' },
            { label: 'Guide', slug: 'guide' },
            { label: 'Scheduling', slug: 'scheduling' },
            { label: 'Deploying', slug: 'deploying' },
          ],
        },
        {
          label: 'Concepts',
          items: [
            { label: 'Architecture', slug: 'architecture' },
            { label: 'Philosophy', slug: 'philosophy' },
            { label: 'Core Constraints', slug: 'core-constraints' },
            { label: 'Architecture Decisions', slug: 'architecture-decisions' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'CLI', slug: 'reference/cli' },
            { label: 'Discovery', slug: 'reference/discovery' },
            { label: 'barca sql', slug: 'reference/sql' },
            { label: 'Agent Skill', slug: 'reference/agent-skill' },
            { label: 'CLI Contract', slug: 'reference/cli-contract' },
            { label: 'Configuration', slug: 'reference/config' },
            { label: 'Remote Storage', slug: 'reference/remote-storage' },
            { label: 'Telemetry', slug: 'reference/telemetry' },
            { label: 'Server API', slug: 'reference/server-api' },
            { label: 'Decorators API', slug: 'reference/api/decorators' },
          ],
        },
        {
          label: 'Patterns',
          items: [
            { label: 'Asset to Asset', slug: 'patterns/01-asset-to-asset' },
            { label: 'Asset to Task', slug: 'patterns/02-asset-to-task' },
            { label: 'Ordering-Only Deps', slug: 'patterns/03-ordering-only-deps' },
            { label: 'Parallel Tasks', slug: 'patterns/04-parallel-tasks' },
            { label: 'Conditional Execution', slug: 'patterns/05-conditional-execution' },
            { label: 'Error Handling', slug: 'patterns/06-error-handling' },
            { label: 'Anti-Patterns', slug: 'patterns/07-anti-patterns' },
          ],
        },
        {
          label: 'Workflows',
          items: [
            { label: 'Single Asset, No Inputs', slug: 'workflows/01-single-asset-no-inputs' },
            { label: 'Single Asset, One Input', slug: 'workflows/02-single-asset-one-input' },
            {
              label: 'Parametrized Assets & Partitions',
              slug: 'workflows/03-parametrized-assets-and-partitions',
            },
            {
              label: 'Asset Continuity: Rename & Move',
              slug: 'workflows/04-asset-continuity-rename-and-move',
            },
            {
              label: 'Schedule-Driven Reconciliation',
              slug: 'workflows/05-schedule-driven-reconciliation-and-effects',
            },
            {
              label: 'Sensors & External Observations',
              slug: 'workflows/06-sensors-and-external-observations',
            },
            {
              label: 'Execution Controls & Ad-Hoc Params',
              slug: 'workflows/09-execution-controls-and-ad-hoc-params',
            },
            {
              label: 'Tasks & Workflow Management',
              slug: 'workflows/10-tasks-and-workflow-management',
            },
          ],
        },
        {
          label: 'Comparisons',
          items: [
            { label: 'Framework Comparison', slug: 'comparisons/framework-comparison' },
            { label: 'vs. Dagster', slug: 'comparisons/dagster' },
            { label: 'vs. Prefect', slug: 'comparisons/prefect' },
            { label: 'vs. cron / systemd', slug: 'comparisons/cron' },
          ],
        },
        {
          label: 'Contributing',
          items: [
            { label: 'Development', slug: 'contributing/development' },
            { label: 'Testing', slug: 'contributing/testing' },
            { label: 'Releases', slug: 'contributing/releases' },
            { label: 'Changelog', slug: 'contributing/changelog' },
          ],
        },
        {
          label: 'RFCs',
          items: [
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
          ],
        },
      ],
    }),
  ],

  adapter: cloudflare(),
});
