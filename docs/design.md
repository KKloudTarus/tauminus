# Design system

The desktop UI draws every color, surface, and control from the tokens and
classes in `apps/desktop/src/app/styles.css`. New UI uses them instead of raw
colors or `current/NN` opacities, so both themes and the phone layout keep
working without extra code.

## Tokens

Each token is a CSS custom property, defined once per theme, and exposed to
Tailwind as a color (`bg-surface-2`, `text-muted`, `border-line`).

| Token | Use |
|---|---|
| `--background` | Window chrome: tab bar, gaps |
| `--surface-1` | Docked panels (host list), inputs, secondary buttons |
| `--surface-2` | Hover and selected rows |
| `--surface-3` | Dialogs, the palette, popovers |
| `--line`, `--line-strong` | Hairlines; strong for control borders |
| `--foreground`, `--muted`, `--faint` | Text levels; all three keep 4.5:1 on every surface |
| `--accent`, `--accent-soft`, `--accent-fg` | Primary action, selection tint, text on accent |
| `--ok`, `--warn`, `--danger` | State |
| `--prod`, `--prod-soft` | Production hosts only |
| `--scrim` | Behind modal content |
| `--terminal-*` | xterm.js theme: background, foreground, cursor, selection, 16 ANSI colors |

`src/app/theme-contrast.test.ts` checks the WCAG contrast of every text token
and ANSI color, and that the two copies of each theme in `styles.css` match.
A palette change that fails it does not ship.

## Component classes

| Class | What it is |
|---|---|
| `btn`, plus `btn-primary`, `btn-danger`, `btn-ghost`, `btn-sm`, `btn-xs` | Buttons |
| `icon-btn` | Square button around one 16 px `<Icon>` from `src/app/icons.tsx` |
| `field` | Text input, select, textarea |
| `panel` | Raised container: border, radius, shadow, opening animation |
| `modal` | Add to a `panel` opened with `showModal()`: stays inside the window and scrolls |
| `modal-footer` | Button row that sticks to the bottom of a scrolling `modal` |
| `overlay` | Backdrop for a dialog that is not modal |
| `disclosure` | A `<details>` section inside a form |
| `chip`, `chip-prod` | Small label; the prod one is red |
| `kbd` | Keyboard shortcut |
| `text-error` | Error message text |

Utilities override these classes, because Tailwind's utilities layer comes
after the components layer.

## Layout rules

- Under 640 px the host list is a drawer over the terminal, and `modal` and
  `overlay > panel` become bottom sheets. Write dialogs once; the stylesheet
  adapts them.
- On a coarse pointer, buttons, icon buttons, and fields grow to 40 px.
- Hover-only actions also show on `pointer-coarse`, since touch has no hover.
- No `backdrop-filter`, no `filter`, no animation that repeats. On Linux the
  webview often composites in software, and every such effect costs a repaint
  over the terminal.
- The terminal never shows ligatures: commands appear as the bytes that run.

## Screenshots

`e2e/screens.spec.ts` captures the main screens at 1440 x 900 and 390 x 844,
light and dark:

```sh
cd apps/desktop
TAUMINUS_SCREENS=/tmp/shots pnpm exec playwright test screens.spec.ts
```
