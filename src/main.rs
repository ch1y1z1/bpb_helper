#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod pck;
mod tweak;

use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use gpui::{
    AppContext, Application, Bounds, Context as GpuiContext, IntoElement, ParentElement, Render,
    SharedString, Styled, Window, WindowBounds, WindowOptions, div, point, px, size,
};
use gpui_component::{
    ActiveTheme as _, Root, StyledExt as _, WindowExt,
    button::{Button, ButtonVariants},
    h_flex,
    input::{Input, InputState},
    notification::NotificationType,
    v_flex,
};
use rfd::FileDialog;
use std::sync::mpsc;
use std::thread;
use tweak::tweak_game_gde;

const DEFAULT_STEAM_PATH: &str = r"C:\Program Files (x86)\Steam\steamapps\common\Backpack Battles";
const DEFAULT_PCK_NAME: &str = "BackpackBattles.pck";

fn main() {
    Application::new().run(|app| {
        gpui_component::init(app);

        let window_options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(Bounds::new(
                point(px(100.), px(100.)),
                size(px(700.), px(240.)),
            ))),
            window_min_size: Some(size(px(520.), px(220.))),
            ..WindowOptions::default()
        };

        app.open_window(window_options, |window, app| {
            let view = app.new(|cx| RootView::new(window, cx));
            app.new(|cx| Root::new(view, window, cx))
        })
        .unwrap();
    });
}

struct RootView {
    game_path: gpui::Entity<InputState>,
    default_detected: bool,
    picker_open: bool,
}

impl RootView {
    fn new(window: &mut Window, cx: &mut GpuiContext<Self>) -> Self {
        let detected_path = detect_default_path();

        let game_path = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder("输入游戏目录或 PCK 文件路径")
                .clean_on_escape();

            if let Some(path) = detected_path.clone() {
                state = state.default_value(path);
            }

            state
        });

        Self {
            game_path,
            default_detected: detected_path.is_some(),
            picker_open: false,
        }
    }
}

impl Render for RootView {
    fn render(&mut self, window: &mut Window, cx: &mut GpuiContext<Self>) -> impl IntoElement {
        let game_path_input = Input::new(&self.game_path).prefix(div().text_sm().child("📁"));

        div()
            .size_full()
            .bg(cx.theme().secondary)
            .child(
                div().mx_auto().p_4().bg(cx.theme().secondary).child(
                    v_flex()
                        .gap_3()
                        .child(
                            h_flex()
                                .justify_between()
                                .items_center()
                                .child(
                                    div()
                                        .text_lg()
                                        .font_semibold()
                                        .child("Backpack Battles 修改工具"),
                                )
                                .child(h_flex().gap_2().children(self.default_hint(cx))),
                        )
                        .child(
                            v_flex()
                                .gap_1()
                                .child(
                                    h_flex()
                                        .gap_2()
                                        .items_center()
                                        .child(div().text_sm().font_semibold().child("游戏路径")),
                                )
                                .child(game_path_input),
                        )
                        .child(
                            h_flex()
                                .gap_2()
                                .justify_end()
                                .child(Button::new("pick").primary().label("选择文件").on_click(
                                    cx.listener(|view, _, window, cx| {
                                        view.on_pick_click(window, cx);
                                    }),
                                ))
                                .child(Button::new("apply").primary().label("应用").on_click(
                                    cx.listener(|view, _, window, cx| {
                                        view.on_apply_click(window, cx);
                                    }),
                                )),
                        ),
                ),
            )
            .children(Root::render_dialog_layer(window, cx))
            .children(Root::render_notification_layer(window, cx))
    }
}

impl RootView {
    fn default_hint(&self, cx: &GpuiContext<Self>) -> Vec<gpui::AnyElement> {
        if self.default_detected {
            vec![
                div()
                    .px_2()
                    .py_1()
                    .rounded(px(6.))
                    .bg(cx.theme().accent)
                    .text_xs()
                    .text_color(cx.theme().accent_foreground)
                    .child("检测到默认游戏路径")
                    .into_any_element(),
            ]
        } else {
            vec![]
        }
    }

    fn set_game_path(&self, path: &str, window: &mut Window, cx: &mut GpuiContext<Self>) {
        self.game_path.update(cx, |input, cx| {
            input.set_value(path.to_string(), window, cx)
        });
    }

    fn current_path(&self, cx: &GpuiContext<Self>) -> String {
        self.game_path.read(cx).value().to_string()
    }

    fn on_apply_click(&mut self, window: &mut Window, cx: &mut GpuiContext<Self>) {
        let input_path = self.current_path(cx);

        let result = resolve_pck_path(&input_path).and_then(|pck_path| {
            let pck_str = pck_path
                .to_str()
                .ok_or_else(|| anyhow!("路径包含非法字符"))?
                .to_string();

            tweak_game_gde(&pck_str).with_context(|| format!("修改失败，文件: {}", pck_str))?;

            Ok::<_, anyhow::Error>(pck_str)
        });

        match result {
            Ok(path) => {
                let msg = format!("修改完成：{}", path);
                window.push_notification((NotificationType::Success, SharedString::from(msg)), cx);
            }
            Err(err) => {
                let message = err.to_string();

                println!("{:?}", err);

                window.open_dialog(cx, move |dialog, _, _| {
                    dialog.title("操作失败").alert().child(message.clone())
                });
            }
        }
    }

    fn on_pick_click(&mut self, _window: &mut Window, cx: &mut GpuiContext<Self>) {
        if self.picker_open {
            return;
        }
        self.picker_open = true;

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let picked = FileDialog::new()
                .add_filter("PCK 文件", &["pck"])
                .pick_file()
                .and_then(|p| p.to_str().map(|s| s.to_string()));
            let _ = tx.send(picked);
        });

        let weak = cx.entity().downgrade();
        cx.spawn(move |_, app: &mut gpui::AsyncApp| {
            let picked = rx.recv().ok().flatten();
            let app_clone = app.clone();
            async move {
                let _ = app_clone.update(|app| {
                    let mut cleared = false;

                    if let Some(path) = picked.clone() {
                        if let Some(window) = app.active_window() {
                            let _ = app.update_window(window, |_, window, cx| {
                                weak.update(cx, |view, cx| {
                                    view.picker_open = false;
                                    view.set_game_path(&path, window, cx);
                                })
                            });
                            cleared = true;
                        }
                    }

                    if !cleared {
                        let _ = weak.update(app, |view, _cx| {
                            view.picker_open = false;
                        });
                    }
                });
            }
        })
        .detach();
    }
}

fn detect_default_path() -> Option<String> {
    let default = PathBuf::from(DEFAULT_STEAM_PATH).join(DEFAULT_PCK_NAME);
    if default.exists() {
        default.to_str().map(|s| s.to_string())
    } else {
        None
    }
}

fn resolve_pck_path(input: &str) -> Result<PathBuf> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("请先输入游戏路径"));
    }

    let path = PathBuf::from(trimmed);
    if path.is_file() {
        return Ok(path);
    }

    if path.is_dir() {
        let candidate = path.join(DEFAULT_PCK_NAME);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    Err(anyhow!(
        "未找到可用的 PCK 文件，请确认路径或手动选择 {}",
        DEFAULT_PCK_NAME
    ))
}
