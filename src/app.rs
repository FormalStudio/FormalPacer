use formalpacer_engine::config::Mode;
use formalpacer_engine::display::Vrr;
use formalpacer_engine::engine::{Command, Game, Snapshot};
use formalpacer_engine::layer::LayerState;
use nd::prelude::*;

use crate::words;

/// The engine's latest snapshot and the way to send it commands. Lives for the whole process;
/// windows come and go around it.
pub struct Model {
    pub snapshot: Option<Snapshot>,
    pub commands: flume::Sender<Command>,
    /// The outcome of the last layer registration, until dismissed.
    pub notice: Option<String>,
}

impl Model {
    pub fn new(commands: flume::Sender<Command>) -> Self {
        Self { snapshot: None, commands, notice: None }
    }

    pub fn apply(&mut self, snapshot: Snapshot) {
        if let Some(notice) = &snapshot.notice {
            self.notice = Some(notice.clone());
        }
        self.snapshot = Some(snapshot);
    }

    fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }
}

pub struct Main {
    model: Entity<Model>,
    _observe: Subscription,
}

impl Main {
    pub fn new(model: Entity<Model>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&model, |_, _, cx| cx.notify());
        Self { model, _observe: observe }
    }

    fn send(&self, command: Command, cx: &mut App) {
        self.model.read(cx).send(command);
    }

    fn banners(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let model = self.model.read(cx);
        let mut out = Vec::new();
        let layer = model.snapshot.as_ref().map(|s| s.layer.clone());
        let missing = match layer {
            Some(LayerState::Missing) => Some("Games aren't paced yet: the Vulkan layer isn't registered"),
            Some(LayerState::Broken(_)) => Some("Games aren't paced: the registered layer's file is gone"),
            _ => None,
        };
        if let Some(title) = missing {
            out.push(
                banner("layer", title)
                    .action("Register")
                    .revealed(true)
                    .on_action(cx.listener(|this, _: &BannerActionClicked, _, cx| {
                        this.send(Command::InstallLayer, cx);
                    }))
                    .into_any_element(),
            );
        }
        if let Some(notice) = model.notice.clone() {
            out.push(
                banner("notice", notice)
                    .action("Dismiss")
                    .revealed(true)
                    .on_action(cx.listener(|this, _: &BannerActionClicked, _, cx| {
                        this.model.update(cx, |m, cx| {
                            m.notice = None;
                            cx.notify();
                        });
                    }))
                    .into_any_element(),
            );
        }
        out
    }

    fn game(&self, snapshot: &Snapshot, game: &Game, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let stat = |value: f64, caption: &'static str| {
            v_flex()
                .flex_1()
                .gap(theme.spacing.xs)
                .child(text(words::fps(value)).text_size(theme.type_scale.title).tabular_nums())
                .child(text(caption).text_size(theme.type_scale.footnote).text_color(theme.text.secondary))
        };
        v_flex()
            .gap(theme.spacing.md)
            .child(
                v_flex()
                    .gap(theme.spacing.xs)
                    .child(
                        text(game.name.clone()).text_size(theme.type_scale.headline).text_color(theme.text.secondary),
                    )
                    .child(text(words::cap(snapshot, game)).text_size(theme.type_scale.large_title)),
            )
            .child(
                h_flex()
                    .gap(theme.spacing.lg)
                    .child(stat(game.stats.avg_fps, "Average fps"))
                    .child(stat(game.stats.low_fps, "1% low"))
                    .child(stat(game.stats.capacity_fps, "Could run at")),
            )
            .into_any_element()
    }

    fn display_section(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> FormSection {
        let theme = cx.theme().clone();
        let displays = snapshot.displays.clone();
        let choices = displays.clone();
        let mut options = vec!["Follow the game".to_owned()];
        options.extend(displays.iter().map(words::display_name));
        let selected = snapshot
            .config
            .display
            .as_ref()
            .and_then(|c| displays.iter().position(|d| &d.connector == c))
            .map_or(0, |i| i + 1);
        let current = snapshot.game.as_ref().and_then(|g| snapshot.display(&g.display)).or_else(|| {
            formalpacer_engine::engine::pick_display(&displays, snapshot.config.display.as_deref(), None, (0, 0))
                .map(|(d, _)| d)
        });

        let mut section = form_section().title("Display").child(form_row("Pace for").child(
            select("display", options).label("Pace for").selected(selected).on_change(cx.listener(
                move |this, e: &SelectChanged, _, cx| {
                    let connector = e.index.checked_sub(1).and_then(|i| choices.get(i)).map(|d| d.connector.clone());
                    this.send(Command::SetDisplay(connector), cx);
                },
            )),
        ));
        if let Some(display) = current {
            let dot = match display.vrr {
                Vrr::Active => theme.success,
                Vrr::Off => theme.warning,
                Vrr::Unsupported => theme.text.tertiary,
            };
            section = section
                .child(form_row("Display").child(label(words::display_name(display)).secondary()))
                .child(form_row("Refresh rate").child(
                    label(format!("{:.2} Hz, {}×{}", display.refresh_hz, display.width, display.height)).secondary(),
                ))
                .child(
                    form_row("VRR").child(
                        h_flex()
                            .gap(theme.spacing.sm)
                            .items_center()
                            .child(div().id("vrr-dot").size(theme.spacing.sm).rounded_full().bg(dot))
                            .child(label(words::vrr(display)).id("vrr").secondary()),
                    ),
                );
        }
        if let Some(game) = &snapshot.game {
            section =
                section.child(form_row("Chosen because").child(label(words::placement(game.placement)).secondary()));
        }
        section
    }

    fn overlay_section(&self, snapshot: &Snapshot, cx: &mut Context<Self>) -> FormSection {
        form_section().title("Overlay").footer(words::OVERLAY_HOTKEY).child(form_row("In-game overlay").child(
            switch("overlay").label("In-game overlay").checked(snapshot.config.overlay).on_toggle(cx.listener(
                |this, e: &Toggled, _, cx| {
                    this.send(Command::SetOverlay(e.checked), cx);
                },
            )),
        ))
    }
}

impl Render for Main {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let snapshot = self.model.read(cx).snapshot.clone();
        let mode = snapshot.as_ref().map_or(Mode::Auto, |s| s.config.mode);
        let banners = self.banners(cx);

        let body = match &snapshot {
            None => status_page("Starting").id("starting").into_any_element(),
            Some(snapshot) => {
                let top = match &snapshot.game {
                    Some(game) => self.game(snapshot, game, cx),
                    None => status_page("No game running")
                        .id("no-game")
                        .icon(Icon::Play)
                        .description("Start a Vulkan or Proton game and FormalPacer picks it up on its first frame.")
                        .compact()
                        .into_any_element(),
                };
                let margin = cx.theme().spacing.content_margin;
                form()
                    .child(div().p(margin).child(top))
                    .child(self.display_section(snapshot, cx))
                    .child(self.overlay_section(snapshot, cx))
                    .into_any_element()
            }
        };

        scaffold()
            .toolbar(toolbar().title("FormalPacer").item(
                segmented("mode", ["Auto", "Off"]).label("Pacing").selected(usize::from(mode == Mode::Off)).on_change(
                    cx.listener(|this, e: &SelectChanged, _, cx| {
                        let mode = if e.index == 0 { Mode::Auto } else { Mode::Off };
                        this.send(Command::SetMode(mode), cx);
                    }),
                ),
            ))
            .content(v_flex().size_full().children(banners).child(body))
    }
}
