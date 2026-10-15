# QC wf_c926ef06-3ca — T03 design system + shell

Reviewer: Claude Opus 5.5 QC workflow (dimension reviewers, 2 skeptics per finding).
Confirmed: 32 · refuted: 4

## Lead report

FIX LIST (deduplicated, ranked by severity; paths are relative to crates/app/src)

HIGH
1. components/drag_region.rs:24-39 (affects every titlebar button)
   Problem: Titlebar buttons sit on top of drag regions, and nothing stops their mouse-down from also reaching the region underneath. This applies to the sidebar toggle (app_sidebar_layout.rs:258-278), the layout toggles (:280), and the tab-bar "+" and tabs (right_panel_tabs.rs:100-106). Any 1px move during a press calls start_window_move. On macOS that runs a modal drag loop that consumes the mouse-up, so the click is lost. A double-click on a button also bubbles to on_double_click and zooms the window. Upstream excludes these with `.drag-region button,input,textarea,select,a { -webkit-app-region:no-drag }` (index.css:1741-1748). The module doc's claim that "interactive children keep their clicks" is wrong.
   Fix:
   - Make interactive elements call `cx.stop_propagation()` on left mouse-down. This covers ui::Button, the sidebar icon buttons, surface tabs, and the absolute toggle containers.
   - Only start a window move once `event.dragging()` is true and the pointer has passed a DRAG_THRESHOLD.
   - Correct the module doc and the PORTING.md:209 row.

MEDIUM
2. components/chat/panel_layout_controls.rs:26-34; add ui/toggle.rs
   Problem: ui/toggle.tsx was not ported, so the titlebar toggles are ghost Buttons. Their icons paint muted_foreground (light #71717a, dark #818181). Upstream Toggle ghost/sm paints them in text-foreground at 0.8 opacity (about #525255 light, #c6c6c6 dark), and in accent-foreground when pressed. Upstream has about 8-10 Toggle consumers that later tasks will need.
   Fix:
   - Port ui/toggle.rs with the full variant/size table (default, pill, ghost, outline, segmented): 28px for sm, rounded-lg, foreground icon at 0.8, pressed state bg-accent plus text-accent-foreground.
   - Use it in PanelLayoutControls.
   - Keep Button::pressed. Upstream Button has data-pressed, so it is faithful.

3. theme/mod.rs:198-221, plus components/sidebar.rs:176-180, 269, 308 and components/sidebar/sidebar_chrome.rs:62, 75, 77
   Problem: The [data-app-sidebar] token scope (index.css:1201-1240, recomputed at :1528) is not applied. In the default dark theme, sidebar text uses the global tokens. "Code" and the branch text render #818181 where upstream renders #a3a3a3, and titles use #f5f5f5 instead of #f1f3f7.
   Fix:
   - Add a sidebar-scoped token set to AppTheme: foreground, muted_foreground, accent, accent_foreground, border, input, muted, card, plus their contrast variants.
   - For the default theme, take the values from [data-app-sidebar]. For themed palettes, use the root values per index.css:1491.
   - Point all sidebar call sites at it. The sidebar_foreground and sidebar_muted_foreground fields already exist.

4. theme/appearance_fonts.rs:33-34, 138
   Problem: On stock macOS, "SF Mono" and "SFMono-Regular" are never in all_font_names() because the face is the hidden .SFNSMono / .AppleSystemUIFontMonospaced. As a result every code surface falls back to Menlo. Upstream's default stack starts with ui-monospace and resolves to SF Mono.
   Fix:
   - Register /System/Library/Fonts/SFNSMono.ttf with text_system().add_fonts, or resolve ui-monospace to the system monospaced face.
   - Make the default stack follow index.css, with ui-monospace first.
   - Update the PORTING.md:201 row.

5. components/app_sidebar_layout.rs:1-4 and components/sidebar/sidebar_chrome.rs:85-136
   Problem: ui/sidebar.tsx (imported by about 20 upstream files, including settings, PR, usage and Sidebar) is not ported as a primitive. Its pieces are merged into app_sidebar_layout.rs alongside AppSidebarLayout.tsx and ChatView.tsx. SidebarMenuButton size="icon" was reinvented as SidebarUtilityButton / sidebar_icon_button. SidebarHeaderIconButton is renamed and lives in the wrong file. SidebarRail is inlined. There is no Deviations row. This breaks PORTING rules 1 and 2, and tasks T15/T19/T20 cannot reuse any of it.
   Fix:
   - Create ui/sidebar.rs with the desktop primitives under their upstream names: SidebarProvider, Sidebar, SidebarRail, SidebarTrigger, SidebarInset, SidebarContent, SidebarGroup, SidebarMenu, SidebarMenuItem, SidebarMenuButton, SidebarFooter, SidebarInput.
   - Move SidebarHeaderIconButton into sidebar_thread_header.rs under its upstream name.
   - Rewire app_sidebar_layout.rs and sidebar_chrome.rs to use these.
   - Log that sidebarState is cut along with mobile.

LOW
6. ui/tooltip.rs:119-142, plus label call sites
   Problem: `.shortcut()` adds a Kbd chip. Upstream appends " (⌘B)" as plain text. The non-glass tooltip is also missing the 1px lip.
   Fix:
   - Make `.shortcut()` append the shortcut to the text, or build the string at each call site.
   - Add tokens::control_lip(dark) to the default tooltip shadow.
   - Copy these strings verbatim from upstream:

     | Location | Change to |
     |---|---|
     | app_sidebar_layout.rs:274 | "Toggle main sidebar" |
     | panel_layout_controls.rs:48 | "Toggle thread details" |
     | panel_layout_controls.rs:55 | "Toggle right panel" |
     | panel_layout_controls.rs:69-70 | "Restore panel size" / "Maximize panel" |
     | composer_surface.rs:141 | "Attach files" |
     | sidebar_chrome.rs:166 | "Pull Requests" |

7. ui/tooltip.rs:117
   Problem: The glass tooltip uses shadow_lg. Upstream uses shadow-xl.
   Fix: Add tokens::shadow_xl (0 20 25 -5 / 0 8 10 -6) and use it at alpha 0.25.

8. components/sidebar.rs:159-201 (branch/diff line in the card row)
   Problem: The row always draws a GitBranch icon, the branch wrapper is not flex_1 (so the diff is not right-aligned), the branch is truncated at the end, and counts print raw ("+3812") with gap_1 between them.
   Fix:
   - Make the branch wrapper flex_1 + min_w_0, with middle truncation.
   - Show FolderGit2 (size-3, muted/40) only for worktree threads.
   - Group digits ("+1,240") and separate +/− with a text space.

9. components/sidebar.rs:305-328 (settled rows)
   Problem: The time label uses full secondary_label instead of secondary_label/0.7 at rest. The monogram is permanently muted at 0.4 and ignores monogram_color. Nothing changes on hover.
   Fix:
   - Show all row text at secondary_label/0.7 at rest and full strength on group hover or focus.
   - Draw the monogram in the project colour at opacity 0.4 plus grayscale at rest, and restore it on hover.

10. components/right_panel_tabs.rs:45, 50, 94 and components/sidebar/sidebar_chrome.rs:165
    Problem: The tab icon is 14px with an extra pl_1. Upstream uses a 16px box with a 12px icon and gap-0.5. The glyphs are also wrong.
    Fix:
    - Use a 16px box with a 12px icon and drop pl_1.
    - The PR tab glyph is Link2 (link-2.svg), per SurfaceIcon "pull-requests".
    - The sidebar footer glyph is GitPullRequestArrow.
    - Register both icons in assets.rs.

11. components/chat/chat_header.rs:56 and components/project_monogram.rs:37-44
    Problem: The header monogram is 16px. Upstream uses size-3.5 (14px). ProjectMonogram has no size option.
    Fix: Add `.size(Rems)` that scales the box, the 25% radius and the font (8.25/16). Use rems(0.875) in ChatHeader.

12. components/chat/composer_surface.rs:49-53
    Problem: The model-picker chevron is 12px muted_foreground.
    Fix:
    - Make the chevron rems(0.875), theme.icon_muted, with -2px horizontal margins.
    - On hover, change the label to text_color(foreground). The chevron itself stays icon_muted.

13. components/chat/composer_surface.rs:153
    Problem: The disabled send button keeps shadow_xs.
    Fix: Remove the shadow in the disabled state. Leave the enabled message-action/24 tint for the composer task.

14. components/app_sidebar_layout.rs:327, 341
    Problem: surface-grain (index.css:1676-1700, ui/sidebar.tsx:241, 260, 334, 589) is neither ported nor logged.
    Fix: Either tile a 256px noise image at 0.035 opacity (128px at 0.02745 for t3-chat) behind the root, sidebar and inset, or add a Deviations row.

15. theme/tokens.rs:240-248, used by button.rs:198
    Problem: control_lip is an inset shadow, so it lands on the opposite edge from upstream (top in light, bottom in dark). It is also painted under the border.
    Fix: Paint an absolutely positioned 1px overlay above the border: bottom edge black/4% in light, top edge white/6% in dark.

16. ui/button.rs:108-121
    Problem: Upstream's base `sm:text-sm` and `sm:[&_svg]:size-4` win on desktop, so a compact button's text is 14px, not 12px.
    Fix:
    - Set Compact text to TEXT_SM and add a comment explaining the sm: cascade.
    - Add an icon-size override to Button. Most upstream icon-micro/icon-tiny call sites pass an explicit size-3, so keep 0.75rem for those and document the choice.

17. components/app_sidebar_layout.rs:140, 147, 197, 236
    Problem: Resizing sets the width to the absolute pointer x, so the edge jumps up to 8px on an off-centre press.
    Fix: On mouse-down, store start_x and start_width. Apply start_width ± (x − start_x). Upstream's 2px "moved" flag only suppresses the click; it does not hold back small deltas.

18. components/app_sidebar_layout.rs:52, 261; components/sidebar/sidebar_chrome.rs:24; components/chat/chat_view.rs:36
    Problem: The 90px traffic-light inset is hard-coded, and upstream drops it to 12px in fullscreen.
    Fix: Derive the inset from window.is_fullscreen() at render time (90 or 12) and pass it down instead of the constants.

19. theme/appearance_fonts.rs:101
    Problem: smoothing defaults to false. Upstream's default is true (contracts settings.ts:424).
    Fix: Default it to true.

20. theme/mod.rs:238
    Problem: The standard dark tool_error_icon uses tw::RED_300 (#ffa2a2). Upstream's literal is #fca5a5 (index.css:1177).
    Fix: Use hex(0xfca5a5).

21. components/sidebar/sidebar_chrome.rs:70-77 and main.rs:35 (PM decision; three findings merged)
    Problem: The brand slot shows the T3 wordmark plus "Code", a faithful copy of upstream SidebarBrandMark. The window title and the product are "Turin". Neither choice is logged in Deviations.
    Fix:
    - PM to decide. If the slot should read "Turin", render it in the same slot and style (text-sm, font-medium, tracking-tight, foreground, h-7 at the content-left).
    - Either way, add one Deviations row covering the brand mark and the window title.
    - Related: resolve_thread_sidebar_minimum_width is never wired. The minimum is fixed at 208px (app_sidebar_layout.rs:139, 154), and the probe is noted only in a module doc. Add a Deviations row saying it must be wired when interface font size becomes configurable.
    - Narrow the blanket #[allow(dead_code)] in main.rs:5-17 so it covers only items kept intentionally.

22. theme/theme_palettes.rs:10, 976 and theme/mod.rs:1, 157, 333
    Problem: MOBILE_DEFAULT_THEME_ID is renamed to DEFAULT_THEME_ID. RESERVED_THEME_IDS and UNPUBLISHABLE_THEME_IDS are dropped. find_theme stands in for getThemeDefinition, and get_theme_colors_for_appearance for getThemeColorsForMode. mod.rs has no "Port of" header, and none of this is logged.
    Fix:
    - Restore the upstream names, or log the renames.
    - Port the two constant sets.
    - Add a Port header naming index.css and themePalette.ts.
    - Add a Deviations row deferring legacy-id aliases, resolveThemeAppearance and themePreview.ts to the settings task (T20).

23. ui/popover.rs:28
    Problem: The width (sm/md/lg plus max-w), variant="panel" and tooltipStyle options are missing. Upstream uses each of them in more than 10 places.
    Fix: Add a PopoverWidth enum plus panel and tooltip_style options, or log the omission.

24. components/sidebar.rs:334-375
    Problem: CollapsibleSectionHeader is reimplemented as SidebarSectionHeader and loses expanded, accessory, tone, variant and status.
    Fix: Port it as ui/collapsible_section_header.rs under its upstream name and use it from sidebar.rs.

25. components/chat/chat_header.rs:1-2
    Problem: The header names the wrong path. WorkspaceBreadcrumb.tsx lives at components/, not components/chat/. It is also inlined into ChatHeader, but settings, usage and PR pages reuse it.
    Fix:
    - Extract it to components/workspace_breadcrumb.rs with a correct header.
    - Add a Deviations note for the ChatComposer and ComposerPrimaryActions frames folded into composer_surface.rs.

26. Tests in components/thread_sidebar_width.rs, theme/appearance_contrast.rs, theme/appearance_fonts.rs:195+ and theme/theme_palettes.rs:983+
    Problem: The upstream it() cases were replaced with tests the agent wrote. Missing cases include:
    - max(800,238)=238, max(1200,238)=560, clamp(208,238,560)=238
    - contrast 135/200/70/100
    - the Bad"Name, M+ 1m, 3270 Nerd Font and " Fira Code , Menlo " cases
    - the stock-palette surface and search-pair assertions

    The logic itself is correct.
    Fix:
    - Port the applicable cases one-to-one under their upstream names.
    - Log the u32 clamp signature, which drops upstream's rounding and NaN fallback.

27. components/sidebar.rs:67-114 and components/chat/chat_header.rs:56 (advisory; only 1 of 2 verifiers upheld it)
    Problem: The placeholder monogram colours are fixed light-mode -600 shades, so they look dim in dark mode.
    Fix: Port PROJECT_ICON_COLORS as a light/dark table resolved with is_dark(). Acceptable to defer to the sidebar task if logged.

VERDICT: FAIL. The drag region (item 1) is high severity because it drops clicks and zooms the window from every titlebar button.

## Confirmed findings (raw)

### [medium] Panel layout toggles paint their icons muted instead of foreground at 80%
- `crates/app/src/components/chat/panel_layout_controls.rs:26` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream PanelLayoutControls.tsx:52-58,147-154 uses `<Toggle variant="ghost" size="sm">`. In ui/toggle.tsx the base class has `text-foreground ... [&_svg:not([class*='opacity-'])]:opacity-80`, and ghost is `border-transparent text-foreground ... data-pressed:bg-accent data-pressed:text-accent-foreground`, so the icon is foreground at 80%. Turin's `toggle()` builds a `ButtonVariant::Ghost` Button. In button.rs that variant is `icon: Some(theme.muted_foreground)`, which is Button's `[--control-icon-color:var(--contrast-muted-foreground)]`. I sampled the screenshots at the right titlebar icon. light.png is #71717a where upstream gives about #525255 (zinc-800 at 80% over #fcfcfc). dark.png is #818181 where upstream gives about #c6c6c6 (#f5f5f5 at 80% over #0a0a0a). In the screenshots the right cluster reads as muted as the sidebar trigger, but upstream draws it clearly darker in light mode and brighter in dark mode.
- Fix: Add a Toggle primitive (ui/toggle.rs) that ports toggleVariants with ghost and sm: 28px, rounded-lg, text-foreground, icon opacity 0.8, pressed bg-accent plus text-accent-foreground. Use it in PanelLayoutControls instead of Button Ghost.

### [medium] Dark-mode sidebar ignores upstream's [data-app-sidebar] token scope, so muted text is #818181 instead of #a3a3a3
- `crates/app/src/theme/mod.rs:198` · dimension: look-and-feel · upheld 2/2
- Evidence: With the default theme (no data-theme-id), index.css:1201-1240 redefines the tokens inside `[data-app-sidebar]` in dark mode: `--foreground: #f1f3f7; --muted-foreground: #a3a3a3; --accent: #191a1d; --border: rgb(255 255 255 / 8%); --input: rgb(255 255 255 / 18%)`. index.css:1529-1530 ("Recompute contrast wherever a subtree owns its own semantic color palette. :root, [data-app-sidebar]") then recomputes the --contrast-* tokens from those values. As a result `text-muted-foreground` and `text-foreground` inside the sidebar resolve to the sidebar values. AppTheme::resolve produces only one global muted_foreground / foreground / accent / border. Sidebar code uses those globals: sidebar_chrome.rs:77 brand "Code" `theme.muted_foreground`, sidebar.rs:176-180 branch `muted_foreground/0.4`, sidebar.rs:269 title `foreground/0.9`, sidebar_chrome.rs:62/75 wordmark `theme.foreground`. I measured "Code" in dark.png at #818181 where upstream is #a3a3a3, and the branch text at #434444 where upstream is about #515151. Light mode is unaffected because the overrides equal the root zinc values. Later sidebar tasks (hover cards, menus, search) will inherit the same wrong values.
- Fix: Resolve a second, sidebar-scoped token set (foreground, muted_foreground, accent, accent_foreground, border, input, muted, card, plus their contrast variants) from the [data-app-sidebar] values. For themed palettes, use the app-theme sidebar mapping from index.css:1491. Expose it as e.g. `theme.sidebar_scope()` or a nested `AppTheme`, and use it for everything rendered inside the sidebar.

### [low] Tooltips render shortcuts as a Kbd chip, and several labels differ from upstream
- `crates/app/src/ui/tooltip.rs:136` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream shows shortcuts as plain text in parentheses inside TooltipPopup, not as a Kbd chip. AppSidebarLayout.tsx:165 is `Toggle main sidebar{shortcutLabel ? ` (${shortcutLabel})` : ""}`. PanelLayoutControls.tsx:68-69 is `Toggle thread details (…)`, :125 is `Toggle right panel (…)`, :140 is `"Restore panel size" : "Maximize panel"`. CommandPalette.tsx:3408 is `Create (${createShortcutLabel})`. Turin appends `Kbd::new(shortcut)` (tooltip.rs:136-142), and its labels differ. app_sidebar_layout.rs:274 has `Tooltip::new("Toggle sidebar").shortcut("⌘B")`. panel_layout_controls.rs:48 has "Thread details", :55 has "Toggle panel", :70 has "Restore panel". composer_surface.rs:141 has "Attach images" where upstream (ChatComposer.tsx:7765) says "Attach files". sidebar_chrome.rs:166 has "Pull requests" where upstream (SidebarChrome.tsx:227) says "Pull Requests". Turin's tooltip also omits the popup lip `before:shadow-[0_1px_black/4%]` (dark: `0_-1px_white/6%`).
- Fix: Make `.shortcut()` format as ` (⌘B)` appended to the text, or drop it and build the string at call sites as upstream does. Copy upstream's tooltip strings verbatim. Add `tokens::control_lip(dark)` to the default tooltip shadow.

### [low] Sidebar card row's branch/diff line differs from upstream (icon, alignment, truncation, number format)
- `crates/app/src/components/sidebar.rs:159` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream Sidebar.tsx:2161-2185: `<ThreadWorktreeIndicator/>` renders a `FolderGit2Icon size-3` only when the thread has a worktreePath (ThreadStatusIndicators.tsx:846-867); otherwise there is no icon. The branch is `<span className="flex min-w-0 flex-1 text-muted-foreground/40"><MiddleTruncate value={thread.branch}/></span>`. The diff is `+{formatDiffCount(n)}</span>{" "}<span>−…`, and formatDiffCount (client-runtime/diffCount.ts) uses Intl.NumberFormat, giving "+1,240". Turin differs in four ways. It always draws IconName::GitBranch (sidebar.rs:178). The branch div is not flex_1, so the diff is not pushed to the right: in light.png row 2 "+3812 −4" sits right after "task/T01-contracts" instead of at the row's right edge. It end-truncates (`.truncate()`, so the screenshot shows "task/T03-design-syst…" where upstream shows a middle ellipsis). It prints raw counts ("+1240", "+3812") with `gap_1` instead of a text space. The module doc says this row geometry is what the sidebar task plugs into.
- Fix: Make the branch container `flex_1 min_w_0` with a middle-truncating text element. Show FolderGit2 (size-3, muted/40) only for worktree threads. Format counts with digit grouping. Separate +/− with a space.

### [low] Brand slot reads "T3 Code" although the product and window title are Turin
- `crates/app/src/components/sidebar/sidebar_chrome.rs:70` · dimension: look-and-feel · upheld 2/2
- Evidence: SidebarChromeHeader renders `svg().path(T3_WORDMARK)` plus `"Code"` (sidebar_chrome.rs:70-77), visible as "T3 Code" at the top of all four screenshots. main.rs:35 already sets the window title to "Turin", and README/PORTING name the product Turin ("t3 fork but without all the bullshit"). Upstream's slot (SidebarChrome.tsx:113-144) is the product mark, a Link to "/" whose width also drives the sidebar minimum (resolveThreadSidebarMinimumWidth: "so 'T3 Code' never clips"). The visible brand therefore contradicts the window title and the product name. No Deviations row records keeping or replacing the T3 brand.
- Fix: PM decision. Either render "Turin" in the same slot and style (`text-sm font-medium tracking-tight`, foreground, h-7 at --workspace-titlebar-content-left) and log a Deviations row, or log the decision to keep the T3 mark. In either case, feed the brand width into resolve_thread_sidebar_minimum_width.

### [low] Header breadcrumb project mark is 16px; upstream is size-3.5 (14px)
- `crates/app/src/components/chat/chat_header.rs:56` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream ChatHeader.tsx:330 uses `<ProjectFavicon project={activeProject} className="size-3.5" />`. The monogram is an SVG with viewBox 16 that scales to its box (ProjectMonogram.tsx:21-30). Turin's ProjectMonogram hardcodes `.size(rems(1.))` and `.text_size(px(8.25))` (project_monogram.rs:37,44) with no size option, so the header crumb renders the 16px sidebar size. In the screenshots the header "T" tile is the same size as the sidebar tiles.
- Fix: Add a `size(Rems)` builder to ProjectMonogram that scales the box, radius (25%) and font (8.25/16 of the size). Use rems(0.875) in ChatHeader.

### [low] Placeholder project colours don't switch to the dark-mode shade
- `crates/app/src/components/sidebar.rs:67` · dimension: look-and-feel · upheld 1/2
- Evidence: Upstream projectIconColors.ts maps every project color to a light/dark pair, e.g. blue `text-blue-600 dark:text-blue-400` and emerald `text-emerald-600 dark:text-emerald-400`. Turin hardcodes `monogram_color: 0x155dfc` (blue-600) and `0x009966` (emerald-600) for both appearances (sidebar.rs:67,79,90,103,114; chat_header.rs:56). In dark.png the "T" tiles are dark blue on near-black, much dimmer than upstream's blue-400 #51a2ff.
- Fix: Port PROJECT_ICON_COLORS as a light/dark table (e.g. in project_monogram.rs) and resolve it with `theme.is_dark()` instead of passing a fixed hex.

### [low] Right-panel tab icon box is 14px plus 4px padding; upstream is a 16px box with a 12px icon
- `crates/app/src/components/right_panel_tabs.rs:45` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream RightPanelTabs.tsx:1218-1228 wraps the surface icon in PanelTabCloseButton (ui/panel-tab-close-button.tsx:22-27): `flex size-4 ... <span className="relative flex size-3">`. The icons are `FileDiff className="size-3"` and `PullRequestGlyph.pullRequest` = GitPullRequestArrowIcon, size-3. The tab uses `gap-0.5`. Turin draws `Icon::new(icon).size(rems(0.875))` (14px), adds `div().pl_1()` (an extra 4px) before the label, and uses IconName::GitPullRequest instead of GitPullRequestArrow (right_panel_tabs.rs:94). The sidebar footer also uses GitPullRequest where upstream SidebarChrome.tsx:226 uses PullRequestGlyph.pullRequest (sidebar_chrome.rs:165). gpui-kit-assets ships git-pull-request-arrow.svg.
- Fix: Render a 16px box containing a 12px icon, drop the label's pl_1, and switch both PR glyphs to IconName::GitPullRequestArrow (add it to AppIcons).

### [low] Composer model-picker chevron is 12px muted; upstream is 14px icon-muted with stroke 2.25
- `crates/app/src/components/chat/composer_surface.rs:49` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream ComposerControl.tsx:73-90, ComposerControlChevron at size sm: `size-3.5 text-icon-muted`, `strokeWidth={2.25}`. The control's `[&_svg]:-mx-0.5` also applies, and the control has `hover:text-foreground`. Turin uses `Icon::new(IconName::ChevronDown).size(rems(0.75)).color(theme.muted_foreground)` with no hover recolor and no 1px border box (`border border-transparent`).
- Fix: Use rems(0.875) and theme.icon_muted with -2px horizontal margin, and add a hover text_color(theme.foreground). If the chevron should match exactly, add a stroke-2.25 chevron asset.

### [low] Settled rows: time label not dimmed to secondary-label/70, and favicon has no hover restore
- `crates/app/src/components/sidebar.rs:328` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream settled rows have variantAction "unsettle", and rowSurfaceClassName (Sidebar.tsx:1592) applies `[&:not(:hover):not(:focus-within)_*]:text-secondary-label/70`, so every descendant at rest is secondary-label/70, including the time label. The favicon is `opacity-40 grayscale` with `group-hover/sidebar-row:opacity-100 group-hover/sidebar-row:grayscale-0` (Sidebar.tsx:1828-1832). Turin colors the time `theme.secondary_label` (measured #818181 in dark.png where upstream gives about #5a5a5a). The monogram is permanently `muted_foreground` at opacity 0.4, so hovering never restores the project color.
- Fix: Use secondary_label at 0.7 for the time label at rest and full strength on group hover. Pass the real project color to the monogram and apply opacity 0.4 plus a gray tint only when the row is not hovered.

### [low] Send button keeps its drop shadow while disabled
- `crates/app/src/components/chat/composer_surface.rs:153` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream ComposerPrimaryActions.tsx:314-317: `shadow-xs ... enabled:shadow-message-action/24 ... disabled:opacity-64 disabled:shadow-none`. Turin renders the empty-draft (disabled) send button with `.shadow(tokens::shadow_xs(0.05)).opacity(0.64)`. The shadow should be absent while disabled, and when enabled it should be tinted message-action/24 rather than black/5.
- Fix: Drop the shadow in the disabled state. When enabled, use shadow_xs tinted with theme.message_action at 0.24 plus inset_control_highlight().

### [low] surface-grain texture on sidebar and body not ported and not logged in Deviations
- `crates/app/src/components/app_sidebar_layout.rs:341` · dimension: look-and-feel · upheld 2/2
- Evidence: Upstream ui/sidebar.tsx:241,260,334 put `bg-sidebar surface-grain` on the sidebar, and index.css:1678-1699 bakes `--surface-grain` (a 256px fractal-noise tile at 3.5% opacity) into the app chrome background. Turin paints flat `.bg(theme.sidebar)` and `.bg(theme.app_chrome_background)`. PORTING.md's Deviations table logs blur, fades, and scale animations as GPUI gaps, but has no row for the grain.
- Fix: Either tile a pre-rendered 256px noise PNG at the upstream opacity behind the sidebar and root backgrounds (GPUI can paint images), or add a Deviations row explaining why it is omitted.

### [low] Button sizes compact / icon-micro / icon-tiny use the mobile text and icon sizes; upstream's desktop cascade gives larger ones
- `crates/app/src/ui/button.rs:108` · dimension: fidelity · upheld 2/2
- Evidence: Upstream's base cva string has `sm:text-sm` and `sm:[&_svg:not([class*='size-'])]:size-4`. The compact size only adds the unprefixed `text-xs` and `[&_svg…]:size-3.5`; icon-micro and icon-tiny only add `[&_svg…]:size-3`. tailwind-merge keeps both classes because their modifiers differ. Compiling them with tailwindcss 4.3.3 (node, /tmp/t03qc/tw.mjs) puts the `@media (width >= 40rem)` rules after the plain ones, so on desktop `.sm:text-sm` and `.sm:[&_svg…]:size-4` win. That makes upstream's desktop values compact = 14px/20px text with 16px icons, and icon-micro / icon-tiny = 16px icons. micro, xs and xl avoid this because they add their own `sm:` overrides. The port has `Self::Compact => … spec(1.75, 7., 0.25, TEXT_XS, 0.875)`, `Self::IconMicro => … square(1.25, 0.75)` and `Self::IconTiny => square(1., 0.75)`, i.e. 12px text and 14px/12px icons. The module's own doc says it uses the desktop (`sm:`) values.
- Fix: Use TEXT_SM and icon 1.0rem for Compact, and icon 1.0rem for IconMicro and IconTiny, so they match the rendered desktop cascade. Add a comment explaining that the base `sm:` rules override these variants' unprefixed classes. If the smaller sizes are kept on purpose, log that in Deviations.

### [medium] Default code font resolves to Menlo on stock macOS, but upstream renders SF Mono
- `crates/app/src/theme/appearance_fonts.rs:138` · dimension: fidelity · upheld 2/2
- Evidence: With no custom code font, upstream removes the `--font-mono` override, so index.css `--font-mono: ui-monospace, "SF Mono", …` applies. In Chromium/Electron on macOS, `ui-monospace` is SF Mono. The port resolves `DEFAULT_CODE_FONT_STACK` (`"SF Mono", "SFMono-Regular", Menlo, …`) against `text_system().all_font_names()`, and `resolve_font_family` maps `ui-monospace`/`monospace` to None. On this machine NSFontManager.availableFontFamilies has no `SF Mono`; the system file /System/Library/Fonts/SFNSMono.ttf is a hidden `.`-prefixed family. So `"SF Mono"` and `"SFMono-Regular"` never match and every code surface falls back to `FALLBACK_MONO_FONT = "Menlo"`. The Deviations row says "Menlo fallback" but not that the upstream default face can never be chosen.
- Fix: Map `ui-monospace` (and SF Mono when it is not installed by name) to the system monospaced face. For example, register /System/Library/Fonts/SFNSMono.ttf via `cx.text_system().add_fonts` at startup and use its family name, or use CoreText's system monospaced UI font. Then resolve the index.css default stack (`ui-monospace` first) when no custom font is set. Update the Deviations row.

### [low] Font smoothing defaults to false; upstream defaults to true
- `crates/app/src/theme/appearance_fonts.rs:101` · dimension: fidelity · upheld 2/2
- Evidence: upstream packages/contracts/src/settings.ts:424 is `fontSmoothing: Schema.Boolean.pipe(Schema.withDecodingDefault(Effect.succeed(true)))`. The Rust `AppearanceFontPreferences::default()` has `smoothing: false`.
- Fix: Default `smoothing` to `true`, matching DEFAULT settings, so the settings task inherits the right default.

### [low] Dark --tool-error-icon for the standard theme uses Tailwind v4 red-300 (#ffa2a2) instead of upstream's literal #fca5a5
- `crates/app/src/theme/mod.rs:238` · dimension: fidelity · upheld 2/2
- Evidence: In upstream index.css's dark `:root` block, `--tool-error-icon: #fca5a5;` is a hard-coded literal (the v3 red-300). The port has `if dark { tw::RED_300.into() }`, and tokens.rs has `RED_300: Rgba = hex(0xffa2a2)`.
- Fix: Use `color::hex(0xfca5a5)` for the standard dark tool error icon.

### [low] Glass tooltip uses shadow-lg instead of upstream's shadow-xl
- `crates/app/src/ui/tooltip.rs:117` · dimension: fidelity · upheld 2/2
- Evidence: Upstream TooltipPopup glass variant: `"dropdown-glass shadow-xl shadow-black/25 before:hidden"`. Tailwind 4 compiles shadow-xl to `0 20px 25px -5px, 0 8px 10px -6px` (checked with the compiler). The port has `.shadow(tokens::shadow_lg(0.25))`, which is `0 10px 15px -3px, 0 4px 6px -4px`.
- Fix: Add `tokens::shadow_xl(alpha)` (0 20 25 -5 / 0 8 10 -6) and use it with alpha 0.25 for the glass tooltip.

### [low] Outline-control 1px lip is drawn on the opposite edge from upstream
- `crates/app/src/theme/tokens.rs:240` · dimension: fidelity · upheld 2/2
- Evidence: Upstream draws `before:shadow-[0_1px_black/4%]` (light) and `dark:before:shadow-[0_-1px_white/6%]` as OUTER shadows of an `inset-0` ::before. That paints the light lip over the bottom border and the dark lip over the top border; the doc comment calls it "the 1px lip under raised surfaces". `control_lip` instead builds an INSET shadow with y=+1 (light) or y=-1 (dark). GPUI documents inset shadows as "drawn inside the element's bounds", so the light lip appears at the top inner edge and the dark one at the bottom: flipped. It is used by Outline and DestructiveOutline (button.rs:198).
- Fix: Draw the lip as a 1px inset shadow at the bottom in light mode (offset y=-1) and at the top in dark mode (y=+1). Alternatively, paint an absolutely positioned 1px child that reproduces the ::before outer shadow.

### [low] Theme-id helpers renamed or dropped without Deviations rows; legacy theme-id aliases not honoured
- `crates/app/src/theme/theme_palettes.rs:976` · dimension: fidelity · upheld 2/2
- Evidence: themePalettes.ts exports `MOBILE_DEFAULT_THEME_ID`, `MOBILE_THEME_IDS`, `RESERVED_THEME_IDS` and `UNPUBLISHABLE_THEME_IDS`. The port renames the first to `DEFAULT_THEME_ID`, drops the others and adds `T3_CODE_THEME`/`find_theme`, with no Deviations row. Upstream themePalette.ts:300 also normalises stored ids through `LEGACY_THEME_ID_ALIASES` (`t3-chat-dark`, `t3-grove`, `t3-ocean`, `t3-ember`, `t3-iris`). `find_theme` matches exact ids only, so an imported T3 settings file with `t3-ocean` silently falls back to the standard palette. `t3-chat-dark` also loses its dark-mode hint (`legacyThemeMode`).
- Fix: Keep `MOBILE_DEFAULT_THEME_ID` or log the rename. Keep `RESERVED_THEME_IDS` (the custom-theme library needs it) or log the omission. Port `normalizeThemeId`/`legacyThemeMode` into `find_theme`/`apply`.

### [low] Sidebar brand renders the T3 wordmark + 'Code' while the window title says 'Turin'; no Deviations entry
- `crates/app/src/components/sidebar/sidebar_chrome.rs:72` · dimension: fidelity · upheld 2/2
- Evidence: Upstream's brand slot (SidebarChrome.tsx `SidebarBrandMark`) is the product's own mark: the T3Wordmark plus muted "Code", and the app name comes from branding.ts `APP_BASE_NAME = … ?? "T3 Code"`. Turin hard-codes `svg().path(T3_WORDMARK)` + `.child("Code")`, but main.rs:35 sets `title: Some("Turin".into())` and PORTING.md names the product Turin. The app now shows two product names, and neither choice is logged.
- Fix: Port branding.ts as an `APP_BASE_NAME = "Turin"` constant. In the brand slot, render that name in the same `text-sm font-medium tracking-tight` style (or a Turin wordmark), and record the rebrand in Deviations. If the PM wants to keep the T3 mark, log that instead.

### [low] Popover port drops the upstream `width`, `variant="panel"` and `tooltipStyle` options
- `crates/app/src/ui/popover.rs:28` · dimension: fidelity · upheld 2/2
- Evidence: Upstream `PopoverPopup` has `width: auto|sm(w-64)|md(w-80)|lg(w-96)` with `max-w-[calc(100vw-2rem)]`, plus `variant: "default"|"panel"` and `tooltipStyle` (rounded-md, text-xs, shadow-md/5, py-1 px-2). The port only exposes `padding`. PORTING rule 3 says to keep small features inside kept code, and the omission is not in Deviations.
- Fix: Add a `PopoverWidth` enum and `tooltip_style`/`panel` options mirroring upstream's class table, or log the omission.

### [high] Titlebar buttons sit on top of the header drag regions. A tiny pointer move during a click moves the window and the click is lost, and a double-click on a button zooms the window
- `crates/app/src/components/drag_region.rs:24` · dimension: correctness · upheld 2/2
- Evidence: drag_region.rs:24-39 arms on `on_mouse_down` and calls `window.start_window_move()` on the first `on_mouse_move` while armed, with no movement threshold. It also adds `.on_double_click(|_, window, _| window.titlebar_double_click())`. In gpui `Interactivity::on_mouse_down`/`on_mouse_move` (gpui-pre div.rs:126-140, 303-313), the only gate is `hitbox.is_hovered(window)`. `Window::hit_test` (window.rs:1165-1177) counts every hitbox under the pointer as hovered until one has `BlockMouseExceptScroll`. Nothing in Turin occludes (`grep occlude|block_mouse|stop_propagation` only hits the two resize handles). `ui::Button` registers only `on_click`, and gpui's click dispatch (div.rs ~2955, ~3105) never calls `stop_propagation`. Three cases follow. (a) The sidebar toggle (app_sidebar_layout.rs:258-278, absolute at x=91 over `SidebarChromeHeader.drag_region`) and the layout toggles (app_sidebar_layout.rs:280, over the chat-header drag region, or over `right-panel-tab-bar` when the panel is open) arm the region below them. (b) The `+` button and surface tabs are children of the tab-bar drag region (right_panel_tabs.rs:100-106) and arm it the same way. (c) macOS `start_window_move` runs `performWindowDragWithEvent:` (gpui-pre-macos window.rs:2220-2231), a modal tracking loop that eats the mouse-up, so the Button's on_click never fires. Two quick clicks on the sidebar toggle also reach the header's on_double_click with click_count 2, so the window zooms. Upstream excludes interactive descendants from dragging: `.drag-region button, … { -webkit-app-region: no-drag }` (index.css:1741-1748).
- Fix: Port upstream's no-drag rule. Interactive elements (ui::Button, the sidebar icon buttons, surface tabs, and the absolute toggle containers in AppSidebarLayout) should stop mouse-down propagation, e.g. `.on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())`. Since gpui runs bubble listeners front-to-back, the region underneath is then never armed and never gets a click. Also gate the move on `event.dragging()` plus a small threshold, as gpui's own DRAG_THRESHOLD does.

### [low] Resize drags set the width to the absolute pointer x instead of start width plus delta, so pressing anywhere off-centre in the hit area makes the width jump
- `crates/app/src/components/app_sidebar_layout.rs:140` · dimension: correctness · upheld 2/2
- Evidence: `on_mouse_move` sets `self.sidebar_width = clamp_thread_sidebar_width(x, …)` (line 140) and `self.right_panel_width = clamp_preview_panel_width(viewport_width - x, max)` (line 147). The sidebar rail is a 16px hit area centred on the edge (`left(px(sidebar_width - 8.))`, `w(px(16.))`). Pressing 7px right of the edge and moving 1px therefore jumps the width by about 8px. A plain click or double-click with a pixel of jitter also changes the width, because there is no movement threshold. Upstream useResizeDrag.ts records `startX` and the start width, applies `width + (pendingX - startX)`, and tracks `moved` with a >2px threshold, so a press never moves the edge. A smaller related problem: the root's `on_mouse_move`/`on_mouse_up` only fire while the root hitbox is hovered (div.rs:210-224, 303-313), so any future occluding overlay would stall a drag.
- Fix: On mouse down, store the start pointer x and the current effective width in the `resizing` state. On move, apply `start_width ± (x - start_x)` and clamp. Ignore moves of 2px or less to match upstream. Prefer window-level listeners (`window.on_mouse_event`) for an in-progress drag so occluding children cannot swallow it.

### [low] The traffic-light inset is hard-coded and does not drop to 0.75rem in fullscreen as upstream does
- `crates/app/src/components/app_sidebar_layout.rs:52` · dimension: correctness · upheld 2/2
- Evidence: `const DESKTOP_WINDOW_CONTROLS_INSET: f32 = 90.;` is always applied (line 261), and so is `WORKSPACE_TITLEBAR_CONTENT_LEFT = 90. + 28. + 12.` (sidebar_chrome.rs:24, used by the sidebar header and the collapsed chat header). Upstream AppSidebarLayout.tsx only applies `--workspace-controls-left: var(--desktop-window-controls-inset, 90px)` when `isMacosDesktop && !isWindowFullscreen`; otherwise it uses the default `calc(env(safe-area-inset-left) + 0.75rem)` (index.css:122). GPUI hides the traffic lights in fullscreen (`move_traffic_light` → `restore_traffic_light`), so Turin leaves a 90px gap before the sidebar toggle and the brand. This is not in the Deviations log.
- Fix: Derive the controls-left value from `window.is_fullscreen()` at render time (90 when windowed, 12 in fullscreen) and pass it to SidebarChromeHeader and ChatView instead of the constants.

### [low] theme/mod.rs makes up its own theme resolution instead of porting apps/web/src/themePalette.ts / hooks/useTheme.ts, with no Port header and no Deviations row
- `crates/app/src/theme/mod.rs:333` · dimension: process · upheld 2/2
- Evidence: The module opens with `//! Turin's design system: ...` and has no `//! Port of upstream/...` line. `apply()` (mod.rs:333) together with `AppearancePreferences` and `theme_palettes::find_theme` take the place of upstream `resolveThemeAppearance`, `getThemeDefinition`, `getThemeColorsForMode`, `getThemeModes`, `singleAppearanceOf` and `canonicalThemePreference` (themePalette.ts:1092-1144, 1565). None of those names survive, and the behaviour is different. Upstream: `definition && getThemeColorsForMode(definition, mode) === null ? definition.appearance : mode`, which flips the app to the theme's own appearance. Turin (mod.rs:157): `get_theme_colors_for_appearance(theme, appearance).unwrap_or(theme.colors)` keeps the requested appearance and paints the other half's colors with the wrong contrast target and gpui-component ThemeMode. Legacy ids such as `t3-chat-dark` and `t3-grove` (canonicalThemePreference) are not resolved either. The only Deviations row for theme/mod.rs covers token flattening. The pure tests in themePalette.test.ts ('stored theme preferences', 'singleAppearanceOf', 'resolves legacy t3-prefixed ids') are not ported.
- Fix: Add `theme/theme_palette.rs` (`//! Port of upstream/apps/web/src/themePalette.ts`) with the resolution helpers under their upstream names: resolve_theme_appearance, get_theme_definition, get_theme_colors_for_mode, get_theme_modes, single_appearance_of, canonical_theme_preference. Make `apply()` call them, and port the matching pure tests. Otherwise, add a Deviations row explaining why the resolution was rewritten and what behaviour differs.

### [medium] ui/sidebar.tsx was not ported as a ui primitive; its pieces are spread across components under new names
- `crates/app/src/components/app_sidebar_layout.rs:1` · dimension: process · upheld 2/2
- Evidence: app_sidebar_layout.rs:1-4 says it ports 'AppSidebarLayout.tsx with the desktop parts of components/ui/sidebar.tsx (`SidebarProvider`, `Sidebar collapsible="offcanvas"`, `SidebarRail`, `SidebarTrigger`) and the chat-column + inline right panel row from ChatView.tsx'. That is three upstream files in one module. `SidebarMenuButton size="icon"` is re-implemented as `SidebarUtilityButton` plus `sidebar_icon_button` in sidebar_chrome.rs:85-136, which also takes the place of SidebarThreadHeader.tsx's exported `SidebarHeaderIconButton` under a different name and in a different file. Upstream ui/sidebar.tsx (850 lines) is imported by 17 files, including settings/SettingsSidebarNav.tsx, routes/settings.tsx, routes/_chat.pull-requests.tsx and usage/UsagePage.tsx, so later tasks cannot reuse any of it. ui/sidebarState.ts (`resolveSidebarState`) and its test ui/sidebar.test.tsx are not ported. None of this has a Deviations row.
- Fix: Create `ui/sidebar.rs` (`//! Port of upstream/apps/web/src/components/ui/sidebar.tsx`) exporting the desktop primitives under their upstream names (SidebarProvider/Sidebar/SidebarRail/SidebarTrigger/SidebarMenuButton/SidebarInset), plus `ui/sidebar_state.rs` with its test. Make app_sidebar_layout.rs and sidebar_chrome.rs use them, and move `SidebarHeaderIconButton` into sidebar_thread_header.rs under its upstream name.

### [low] Upstream logic tests are replaced with invented ones; most upstream cases are not ported
- `crates/app/src/theme/appearance_fonts.rs:208` · dimension: process · upheld 2/2
- Evidence: threadSidebarWidth.test.ts has 5 cases. The Rust module (thread_sidebar_width.rs tests) has 4 different ones and drops `resolveThreadSidebarMaximumWidth(800, 238) == 238`, `(1200, 238) == 560` and the raised-minimum clamp `clampThreadSidebarWidth(208, 238, 560) == 238`. appearanceContrast.test.ts checks 135/200/70/100; Rust checks 80/160 and an identity case. appearanceFonts.test.ts has 15 cases; Rust has 5. Missing upstream assertions include `cssFontFamilies('Bad"Name') == '"BadName"'`, `'M+ 1m'`, `'3270 Nerd Font'`, `' Fira Code , Menlo '`, `clampCodeFontSize(13.4) == 13`, `clampInterfaceFontSize(NaN) == 16` and resolveDefaultFamilyLabel. The pure palette-data checks in themePalette.test.ts ('keeps stock dark controls in the neutral-black surface hierarchy', 'keeps the stock sidebar and chat on distinct surfaces in both appearances', search-pair contrast) are also absent, although Turin uses T3_CODE_*_THEME_COLORS directly. PORTING rule 5 says to port tests for pure logic. The gate passes (20 tests) only because these cases were never written.
- Fix: Port each upstream `it(...)` case one-to-one with the same inputs and expected values (thread_sidebar_width, appearance_contrast, appearance_fonts, and the stock palette assertions from themePalette.test.ts), keeping the upstream describe/it names as the test fn names.

### [low] ui/toggle.tsx and ui/collapsible-section-header.tsx are not ported as primitives; Toggle is faked by restyling Button
- `crates/app/src/components/chat/panel_layout_controls.rs:26` · dimension: process · upheld 2/2
- Evidence: panel_layout_controls.rs:26-34: `fn toggle(...) -> Button { Button::new(id).variant(ButtonVariant::Ghost).size(ButtonSize::IconSm).icon(icon).pressed(pressed).tooltip(tooltip).rounded(RADIUS_LG) }`. Upstream PanelLayoutControls uses `<Toggle variant="ghost" size="sm">` from ui/toggle.tsx (variants default/pill/ghost/outline/segmented; ghost pressed is `bg-accent`). Upstream has 8 Toggle consumers. Button gained a `pressed` state that upstream Button lacks (button.rs:356-357). `CollapsibleSectionHeader` (ui/collapsible-section-header.tsx, 3 upstream consumers) is re-implemented inside components/sidebar.rs:334-375 as `SidebarSectionHeader`, so the name is not kept. Neither has a Deviations row.
- Fix: Add `ui/toggle.rs` (port of toggle.tsx with its variant and size table) and `ui/collapsible_section_header.rs` (CollapsibleSectionHeader + SectionHeaderStatus), and use them from panel_layout_controls.rs and sidebar.rs. Then drop `Button::pressed`, or log it as a deviation.

### [low] theme_palettes.rs / themePreview.ts port renames and drops upstream exports without Deviations rows
- `crates/app/src/theme/theme_palettes.rs:10` · dimension: process · upheld 2/2
- Evidence: Upstream `export const MOBILE_DEFAULT_THEME_ID = "t3-code"` becomes `pub const DEFAULT_THEME_ID` (theme_palettes.rs:10). `RESERVED_THEME_IDS` and `UNPUBLISHABLE_THEME_IDS` (small constants used by the custom-theme and import paths) are omitted. `MOBILE_THEME_IDS` is omitted, which is fine because mobile is cut, but it is not logged. `T3_CODE_THEME` and `find_theme` are Turin additions. color.rs:1 claims to be a 'Port of the color math in upstream/packages/shared/src/themePreview.ts', but that file's public API (`STANDARD_THEME_PREVIEW_COLORS`, `THEME_PREVIEW_RENDER_SPECS`, `mixThemePreviewBase`, used by the kept Settings → Appearance theme picker) has no module. PORTING rule 3 says to keep small features even if unused, and rule 4 says to log any place not ported faithfully.
- Fix: Keep the upstream name (`MOBILE_DEFAULT_THEME_ID`, with an alias if wanted), port RESERVED_THEME_IDS and UNPUBLISHABLE_THEME_IDS, and add `theme/theme_preview.rs` (`//! Port of upstream/packages/shared/src/themePreview.ts`) that uses color.rs. Add a Deviations row for anything intentionally deferred.

### [low] Merged or misplaced upstream files: WorkspaceBreadcrumb path is wrong, ChatComposer/ComposerPrimaryActions are folded in, and the useResizableWidth clamp is in the wrong hook
- `crates/app/src/components/chat/chat_header.rs:1` · dimension: process · upheld 2/2
- Evidence: chat_header.rs:1-2: '//! Port of upstream/apps/web/src/components/chat/ChatHeader.tsx and WorkspaceBreadcrumb.tsx'. The real path is components/WorkspaceBreadcrumb.tsx, which settings/SettingsBreadcrumb.tsx, usage/UsagePage.tsx and routes/_chat.pull-requests.tsx also use. composer_surface.rs:1-3 folds in chat/ChatComposer.tsx (7,852 lines) and ComposerPrimaryActions.tsx (399 lines). use_preview_panel_inline_size.rs:29 holds '/// `useResizableWidth`'s clamp.' from hooks/useResizableWidth.ts, which has its own test file, and omits upstream's `if (!Number.isFinite(value)) return defaultWidth`. This breaks PORTING rule 2 (one upstream file → one module, diffable by header).
- Fix: Split these into `components/workspace_breadcrumb.rs`, `components/chat/chat_composer.rs` / `composer_primary_actions.rs` (static frame for now) and `hooks/use_resizable_width.rs` (clamp with the non-finite fallback), each with a correct `//! Port of upstream/...` header.

### [low] Brand identity is inconsistent and not logged: window title says Turin, sidebar brand slot says T3 Code
- `crates/app/src/components/sidebar/sidebar_chrome.rs:71` · dimension: process · upheld 1/2
- Evidence: main.rs:35 sets `title: Some("Turin".into())`, but sidebar_chrome.rs:71-77 renders `svg().path(T3_WORDMARK)` + `"Code"`, and theme_palettes.rs:952 labels the standard palette "T3 Code". Upstream SidebarBrandMark (SidebarChrome.tsx:128-143) hard-codes `<T3Wordmark/>` + 'Code' rather than reading APP_BASE_NAME, so the slot is faithful to upstream. Showing the product as Turin (tagline in PORTING.md) is a product deviation either way, and neither the title rename nor the brand slot has a Deviations row. branding.ts / branding.logic.ts (APP_BASE_NAME, APP_STAGE_LABEL, formatAppDisplayName, with pure tests in branding.test.ts 'branding logic') are not ported, and the stage badge is missing.
- Fix: Have the PM decide. If Turin: render a 'Turin' brand mark in SidebarBrandMark (keeping upstream geometry), set the APP_BASE_NAME equivalent in a ported `branding.rs`, and log it in Deviations. If T3 Code stays: log that the window title differs. Either way, port branding.logic.ts with its tests.

### [low] Brand-width minimum is not wired up, and blanket #[allow(dead_code)] hides it and about 63 other unused items
- `crates/app/src/main.rs:5` · dimension: process · upheld 2/2
- Evidence: main.rs:5-17 puts `#[allow(dead_code)]` on the whole `theme`, `ui`, `components` and `hooks` modules. Building a copy with those attributes removed gives 63 dead-code warnings, including `function resolve_thread_sidebar_minimum_width is never used` (thread_sidebar_width.rs:9). app_sidebar_layout.rs:139 and :154 pass the fixed `THREAD_SIDEBAR_MIN_WIDTH` where upstream passes `resolveThreadSidebarMinimumWidth(brandWidth)` from SidebarBrandWidthProbe. The missing probe is mentioned only in the sidebar_chrome.rs module doc, not in the Deviations table.
- Fix: Add a Deviations row (brand-width probe not ported, minimum fixed at 208px), or measure the brand and pass it through. Narrow the allows to the specific items that are kept intentionally, so new dead code still triggers clippy -D warnings.
