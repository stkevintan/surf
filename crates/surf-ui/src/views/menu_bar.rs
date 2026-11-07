//! The in-window menu bar, for platforms where the OS doesn't draw one.
//!
//! It draws `menu::owned_menus()`, the same definition `menu::init` hands to
//! GPUI, so the native bar and this one can't drift. Clicking an item
//! dispatches its action to the window, exactly as the native bar does, and
//! the shortcut on the right comes from the keymap.

use gpui::{
    Context, IntoElement, MouseButton, MouseDownEvent, OwnedMenu,
    OwnedMenuItem, ParentElement, SharedString, Styled, Window, deferred, div, prelude::*, px,
};

use crate::shell::SurfApp;
use crate::theme::{color, font, radius, size};

/// Menu titles, and the open menu's items below it.
pub fn render(app: &SurfApp, window: &mut Window, cx: &mut Context<SurfApp>) -> impl IntoElement {
    let menus = &app.ui.menus;
    let open = app.ui.menu_open.filter(|i| *i < menus.len());
    let titles: Vec<gpui::AnyElement> = menus
        .iter()
        .enumerate()
        .map(|(i, menu)| title(app, menu, i, open == Some(i), window, cx).into_any_element())
        .collect();

    div()
        .flex()
        .flex_row()
        .flex_none()
        .items_center()
        .h(px(size::MENU_BAR_H))
        .px(px(6.0))
        .bg(color::CHROME)
        .border_b_1()
        .border_color(color::LINE)
        .text_size(px(font::BODY))
        .children(titles)
}

fn title(
    app: &SurfApp,
    menu: &OwnedMenu,
    index: usize,
    open: bool,
    window: &mut Window,
    cx: &mut Context<SurfApp>,
) -> impl IntoElement {
    let items: Vec<gpui::AnyElement> = if open {
        menu.items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| row(app, item, index, i, window, cx))
            .collect()
    } else {
        Vec::new()
    };

    div()
        .id(SharedString::from(format!("menu-{index}")))
        .relative()
        .flex_none()
        .h(px(size::MENU_BAR_H - 4.0))
        .px(px(9.0))
        .flex()
        .items_center()
        .rounded(px(radius::MD - 1.0))
        .when(open, |d| {
            d.bg(color::ACCENT_BG).text_color(color::ACCENT_FG)
        })
        .when(!open, |d| {
            d.text_color(color::TEXT_SECONDARY)
                .hover(|d| d.bg(color::HOVER).text_color(color::TEXT_PRIMARY))
        })
        .cursor_pointer()
        .child(menu.name.clone())
        // Mouse down, not click: a menu bar opens on press, like the native one.
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |app, _: &MouseDownEvent, _, cx| {
                cx.stop_propagation();
                app.ui.menu_open = if app.ui.menu_open == Some(index) {
                    None
                } else {
                    Some(index)
                };
                cx.notify();
            }),
        )
        // With one menu open, hovering another switches to it.
        .on_mouse_move(cx.listener(move |app, _: &gpui::MouseMoveEvent, _, cx| {
            if app.ui.menu_open.is_some_and(|i| i != index) {
                app.ui.menu_open = Some(index);
                cx.notify();
            }
        }))
        .when(open, |d| {
            d.child(deferred(
                div()
                    .absolute()
                    .top(px(size::MENU_BAR_H - 2.0))
                    .left_0()
                    .w(px(size::MENU_W))
                    .flex()
                    .flex_col()
                    .py(px(4.0))
                    .rounded(px(radius::MD))
                    .bg(color::RAISED)
                    .border_1()
                    .border_color(color::LINE_STRONG)
                    .text_color(color::TEXT_PRIMARY)
                    .children(items),
            ))
        })
}

/// One line of an open menu. Returns None for items with nothing to show
/// here: a submenu the OS fills in (Services), or a nested submenu, which
/// Surf's own menus don't use.
/// A menu item's shortcut, as the keymap has it, or empty.
pub fn shortcut(action: &dyn gpui::Action, window: &Window) -> String {
    if window.bindings_for_action(action).is_empty() {
        String::new()
    } else {
        window.keystroke_text_for(action)
    }
}

fn row(
    app: &SurfApp,
    item: &OwnedMenuItem,
    menu: usize,
    index: usize,
    window: &mut Window,
    cx: &mut Context<SurfApp>,
) -> Option<gpui::AnyElement> {
    match item {
        OwnedMenuItem::Separator => Some(
            div()
                .flex_none()
                .my(px(4.0))
                .mx(px(8.0))
                .h(px(1.0))
                .bg(color::LINE)
                .into_any_element(),
        ),
        OwnedMenuItem::Action { name, action, .. } => {
            // `keystroke_text_for` falls back to the action's name, which is
            // not a shortcut; show nothing when there is no binding.
            let keys = shortcut(action.as_ref(), window);
            let check = crate::menu::checked(action.as_ref(), &app.ui, app.session.engine.playing);
            let action = action.boxed_clone();
            Some(
                div()
                    .id(SharedString::from(format!("menu-{menu}-{index}")))
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.0))
                    .pl(px(6.0))
                    .pr(px(10.0))
                    .py(px(3.0))
                    .cursor_pointer()
                    .hover(|d| d.bg(color::ACCENT_BG))
                    // A fixed gutter, so names line up whether or not a
                    // row can be checked.
                    .child(
                        div()
                            .flex_none()
                            .w(px(12.0))
                            .text_color(color::ACCENT_FG)
                            .child(if check == Some(true) { "✓" } else { "" }),
                    )
                    .child(div().flex_1().mr(px(10.0)).child(name.clone()))
                    .when(!keys.is_empty(), |d| {
                        d.child(
                            div()
                                .font_family(font::MONO)
                                .text_size(px(font::SMALL))
                                .text_color(color::TEXT_TERTIARY)
                                .child(keys),
                        )
                    })
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |app, _: &MouseDownEvent, window, cx| {
                            cx.stop_propagation();
                            app.ui.menu_open = None;
                            window.dispatch_action(action.boxed_clone(), cx);
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
            )
        }
        OwnedMenuItem::Submenu(_) | OwnedMenuItem::SystemMenu(_) => None,
    }
}
