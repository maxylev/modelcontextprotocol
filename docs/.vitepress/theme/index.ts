import type { Theme } from 'vitepress'
import DefaultTheme from 'vitepress/theme'

import './custom.css'

/**
 * Custom theme: the default VitePress theme plus the site-specific styling
 * in custom.css. No components, no plugins — kept deliberately small and
 * maintainable.
 */
export default {
  extends: DefaultTheme,
} satisfies Theme
