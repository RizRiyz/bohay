// The bohay website: a custom product landing at `/` (src/pages/index.astro)
// plus Starlight documentation under `/docs/…` (all content lives in the
// `docs/` subfolder of the content collection, so its slugs — and URLs — are
// prefixed with /docs/ and the root stays free for the landing page).
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';

export default defineConfig({
  site: 'https://bohay.dev',
  integrations: [
    starlight({
      title: 'bohay',
      description:
        'The terminal workspace for AI coding agents — run Claude Code, Copilot, Codex, and opencode side by side with a live view of every agent.',
      logo: { src: './src/assets/logo.svg', alt: 'bohay' },
      favicon: '/favicon.svg',
      social: [
        { icon: 'github', label: 'GitHub', href: 'https://github.com/RizRiyz/bohay' },
      ],
      customCss: [
        '@fontsource-variable/inter',
        '@fontsource-variable/jetbrains-mono',
        '@fontsource/ibm-plex-mono/500.css',
        '@fontsource/ibm-plex-mono/600.css',
        '@fontsource/ibm-plex-mono/700.css',
        './src/styles/custom.css',
      ],
      sidebar: [
        {
          label: 'Getting Started',
          items: [
            { label: 'Quickstart', slug: 'docs' },
            { label: 'Installation', slug: 'docs/getting-started/installation' },
            { label: 'Your First Session', slug: 'docs/getting-started/first-session' },
            { label: 'Core Concepts', slug: 'docs/getting-started/concepts' },
          ],
        },
        {
          label: 'Guides',
          items: [
            { label: 'Working with Agents', slug: 'docs/guides/agents' },
            { label: 'Multi-Agent Orchestration', slug: 'docs/guides/orchestration' },
            { label: 'The Git Tab', slug: 'docs/guides/git' },
            { label: 'Worktrees', slug: 'docs/guides/worktrees' },
            { label: 'Remote Sessions', slug: 'docs/guides/remote' },
            { label: 'Scrollback & Copy', slug: 'docs/guides/scrollback' },
            { label: 'Settings & Theming', slug: 'docs/guides/settings' },
            { label: 'Scripting bohay', slug: 'docs/guides/scripting' },
          ],
        },
        {
          label: 'Extending',
          items: [
            { label: 'Using Modules', slug: 'docs/extend/using-modules' },
            { label: 'Writing a Module', slug: 'docs/extend/writing-modules' },
          ],
        },
        {
          label: 'Reference',
          items: [
            { label: 'CLI Commands', slug: 'docs/reference/cli' },
            { label: 'Socket API', slug: 'docs/reference/api' },
            { label: 'Keybindings', slug: 'docs/reference/keybindings' },
            { label: 'Configuration', slug: 'docs/reference/configuration' },
            { label: 'Supported Agents', slug: 'docs/reference/agents' },
          ],
        },
        {
          label: 'Explanation',
          items: [
            { label: 'Architecture', slug: 'docs/explanation/architecture' },
            { label: 'Security Model', slug: 'docs/explanation/security' },
          ],
        },
        { label: 'FAQ & Troubleshooting', slug: 'docs/faq' },
      ],
    }),
  ],
});
