import { defineConfig } from 'vitepress'

const repo = 'https://github.com/maxylev/modelcontextprotocol'

/**
 * The generated rustdoc is copied into the site output by
 * .github/workflows/docs.yml (see docs/ci-publishing.md). The link is
 * base-relative: the router and the markdown pipeline both prefix `/` links
 * with `base`, so the resolved URL is /modelcontextprotocol/rustdoc/... .
 */
const rustdoc = '/rustdoc/modelcontextprotocol/index.html'

const nav = [
  { text: 'Getting Started', link: '/getting-started' },
  {
    text: 'Servers',
    items: [
      { text: 'Filesystem', link: '/servers/filesystem' },
      { text: 'Fetch', link: '/servers/fetch' },
      { text: 'Memory', link: '/servers/memory' },
      { text: 'Shell', link: '/servers/shell' },
    ],
  },
  { text: 'CLI', link: '/cli' },
  { text: 'Protocol', link: '/protocol' },
  { text: 'Security', link: '/security' },
  {
    text: 'Project',
    items: [
      { text: 'Architecture', link: '/architecture' },
      { text: 'Development', link: '/development' },
      { text: 'CI & Publishing', link: '/ci-publishing' },
      { text: 'Verification', link: '/verification' },
      { text: 'OpenRouter E2E', link: '/openrouter-e2e' },
      { text: 'Coverage matrix', link: '/coverage' },
      { text: 'Rust API docs', link: rustdoc },
    ],
  },
]

const sidebar = [
  {
    text: 'Start here',
    items: [
      { text: 'Overview', link: '/' },
      { text: 'Getting started', link: '/getting-started' },
      { text: 'Command line (CLI)', link: '/cli' },
    ],
  },
  {
    text: 'Servers',
    items: [
      { text: 'Filesystem server', link: '/servers/filesystem' },
      { text: 'Fetch server', link: '/servers/fetch' },
      { text: 'Memory server', link: '/servers/memory' },
      { text: 'Shell server', link: '/servers/shell' },
    ],
  },
  {
    text: 'Protocol & security',
    items: [
      { text: 'Protocol', link: '/protocol' },
      { text: 'Security model', link: '/security' },
    ],
  },
  {
    text: 'Project',
    items: [
      { text: 'Architecture', link: '/architecture' },
      { text: 'Development', link: '/development' },
      { text: 'CI & publishing', link: '/ci-publishing' },
      { text: 'Verification', link: '/verification' },
      { text: 'OpenRouter E2E', link: '/openrouter-e2e' },
      { text: 'Coverage matrix', link: '/coverage' },
      { text: 'Rust API docs', link: rustdoc },
    ],
  },
]

export default defineConfig({
  lang: 'en-US',
  title: 'modelcontextprotocol',
  description:
    'Filesystem, Fetch, Memory, and Shell MCP servers in a single Rust binary, implementing the Model Context Protocol 2026-07-28 specification.',
  base: '/modelcontextprotocol/',
  cleanUrls: true,
  lastUpdated: true,
  /**
   * The rustdoc output does not exist when the VitePress build runs; the
   * publishing workflow copies target/doc/ into the built site afterwards
   * (see docs/ci-publishing.md). The link is valid in the deployed site.
   */
  ignoreDeadLinks: [/^\/rustdoc\//],
  head: [
    ['meta', { name: 'theme-color', content: '#5b7cfa' }],
    [
      'meta',
      {
        name: 'description',
        content:
          'Four Model Context Protocol servers (filesystem, fetch, memory, shell) in one Rust binary. Documentation, CLI reference, protocol details, and security model.',
      },
    ],
  ],
  themeConfig: {
    nav,
    sidebar,
    outline: { level: [2, 3], label: 'On this page' },
    docFooter: { prev: 'Previous page', next: 'Next page' },
    search: {
      provider: 'local',
      options: {
        detailedView: true,
        translations: {
          button: { buttonText: 'Search docs', buttonAriaLabel: 'Search docs' },
          modal: {
            noResultsText: 'No results found',
            resetButtonTitle: 'Clear query',
            footer: {
              selectText: 'to select',
              navigateText: 'to navigate',
              closeText: 'to close',
            },
          },
        },
      },
    },
    socialLinks: [{ icon: 'github', link: repo, ariaLabel: 'GitHub repository' }],
    editLink: {
      pattern: `${repo}/edit/main/docs/:path`,
      text: 'Edit this page on GitHub',
    },
    lastUpdated: { text: 'Last updated' },
    darkModeSwitchLabel: 'Appearance',
    sidebarMenuLabel: 'Menu',
    returnToTopLabel: 'Back to top',
    externalLinkIcon: true,
    footer: {
      message: 'Released under the MIT License.',
      copyright: 'modelcontextprotocol documentation',
    },
  },
})
