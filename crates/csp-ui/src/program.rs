//! The progression surface (PLAN §3.2–§3.6): one "Program" window that
//! makes the whole fly → science → research → unlock loop visible and
//! playable — program economy (funds / banked science / contracts), the
//! tech tree with spend-science buttons, the focused vessel's experiment
//! modules with run/transmit/recover actions, and per-part reliability
//! state. Pure egui over plain-data mirrors populated by `csp-view`'s
//! `stage_program` — this crate never sees `csp-sim` (§2.1).

/// A tech-tree node row (the `ResearchTech` target).
#[derive(Clone, Debug)]
pub struct TechRow {
    /// `TechNodeId` as a string.
    pub id: String,
    /// Loc-resolved display name (or the id).
    pub name: String,
    /// `science_cost`.
    pub cost: f64,
    /// Already bought.
    pub researched: bool,
    /// Every `requires` edge researched.
    pub prereqs_met: bool,
    /// `banked >= cost`.
    pub affordable: bool,
}

/// An upgrade-track row (the `ResearchUpgradeLevel` target).
#[derive(Clone, Debug)]
pub struct TrackRow {
    /// `UpgradeTrackId` as a string.
    pub id: String,
    /// Loc-resolved display name (or the id).
    pub name: String,
    /// Levels already purchased.
    pub level: u8,
    /// Cap the researched nodes grant (`0` = not purchasable yet).
    pub cap: u8,
    /// Authored level count — `level == levels` is maxed.
    pub levels: u8,
    /// Cost of the next level, when one is purchasable.
    pub next_cost: Option<f64>,
    /// `banked >= next_cost`.
    pub affordable: bool,
}

/// A contract-board offer (the `AcceptContract` target).
#[derive(Clone, Debug)]
pub struct OfferRow {
    /// The offer index `AcceptContract` names.
    pub index: u64,
    /// Human-readable summary (template + params).
    pub label: String,
}

/// One experiment module aboard the focused vessel.
#[derive(Clone, Debug)]
pub struct ExperimentRow {
    /// The `PartNodeId` carrying the module.
    pub node: u64,
    /// Display name of the part (loc-resolved or part id).
    pub part: String,
    /// `ExperimentId` as a string.
    pub experiment: String,
    /// `data_units` per run.
    pub data_units: f64,
    /// `reusable` — a one-shot already run reads "spent".
    pub reusable: bool,
    /// Already consumed (one-shot markers).
    pub spent: bool,
    /// The module's `SituationMask` contains the current situation AND
    /// a cap is authored for `(body, situation)` — i.e. a
    /// `RunExperiment` would apply right now.
    pub runnable: bool,
    /// Why it cannot run ("" when runnable).
    pub blocked: String,
}

/// One pooled data record aboard the focused vessel (a `DataKey`).
#[derive(Clone, Debug)]
pub struct DataRow {
    /// Opaque row index — the command references the staged key by it.
    pub index: usize,
    /// `exp @ biome / body · situation`.
    pub label: String,
    /// Units pooled.
    pub units: f64,
    /// Science a transmit would bank (`units · transmit_ratio`).
    pub transmit_yield: f64,
    /// Science a surface recovery would bank (1.0 or the `1−ratio`
    /// remainder for a transmitted key).
    pub recover_yield: f64,
    /// The key was already transmitted (recover banks the remainder).
    pub transmitted: bool,
}

/// One part's reliability state on the focused vessel.
#[derive(Clone, Debug)]
pub struct ReliabilityRow {
    /// `PartNodeId` — display only.
    pub node: u64,
    /// Display name of the part.
    pub part: String,
    /// Applied `FailureMode` names.
    pub failures: Vec<String>,
    /// Ignition misfire latched (no thrust until throttle returns to 0).
    pub misfire: bool,
    /// Live thrust multiplier (`< 1.0` = degraded/dead).
    pub thrust_factor: f64,
    /// Λ/θ fraction toward the next failure, `None` when θ is infinite.
    pub wear: Option<f64>,
    /// Dormant-class accrual is in force.
    pub dormant: bool,
}

/// A recent reliability/lab event line from the sim's notification log.
#[derive(Clone, Debug)]
pub struct NoticeRow {
    /// Sim tick the notice was recorded at.
    pub tick: u64,
    /// Formatted line ("vessel 2 part 4 — PART FAILED SensorFault").
    pub text: String,
}

/// Everything `program_panel` needs — all plain data (§2.1).
#[derive(Clone, Debug, Default)]
pub struct ProgramInput {
    /// Program funds (whole funds, not micros — `csp-view` converts).
    pub funds: f64,
    /// Live science total (banked + in-flight project accrual).
    pub science: f64,
    /// Spendable banked science.
    pub banked: f64,
    /// Contract offers on the board.
    pub offers: Vec<OfferRow>,
    /// Active contract summaries.
    pub active: Vec<String>,
    /// The tech tree — `None` for a world with no tree installed.
    pub tech: Option<Vec<TechRow>>,
    /// Upgrade tracks (empty without a tree).
    pub tracks: Vec<TrackRow>,
    /// The focused vessel's science context, when one is staged:
    /// `(vessel label, situation name, biome label or "", body label)`.
    pub context: Option<(String, String, String, String)>,
    /// Experiment modules aboard the focused vessel.
    pub experiments: Vec<ExperimentRow>,
    /// Pooled data records aboard.
    pub pooled: Vec<DataRow>,
    /// The focused vessel is `SurfaceLatched` — gates the Recover
    /// buttons (`RecoverData` requires it).
    pub latched: bool,
    /// Per-node reliability rows for the focused vessel.
    pub reliability: Vec<ReliabilityRow>,
    /// Recent notifications (newest last), already capped.
    pub notices: Vec<NoticeRow>,
}

/// A request the panel emitted — translated into `SimCommand`s by
/// `csp-view` (the §2.1 rule: this crate cannot name sim types; the
/// context fields the sim commands need — biome, situation, the
/// `DataKey` — are resolved view-side from the staged mirror).
#[derive(Clone, Debug)]
pub enum ProgramCommand {
    /// Spend banked science on a tech node → `ResearchTech`.
    ResearchTech {
        /// The node id.
        node: String,
    },
    /// Buy the next upgrade-track level → `ResearchUpgradeLevel`.
    ResearchTrack {
        /// The track id.
        track: String,
    },
    /// Take a contract off the board → `AcceptContract`.
    AcceptContract {
        /// The offer index.
        offer: u64,
    },
    /// Run an experiment module in the current context →
    /// `RunExperiment` (view supplies vessel/biome/situation).
    RunExperiment {
        /// The module's graph node.
        node: u64,
        /// The experiment kind.
        experiment: String,
    },
    /// Bank a pooled record remotely → `TransmitData`.
    Transmit {
        /// The pooled-row index.
        row: usize,
    },
    /// Bank a pooled record by physical recovery → `RecoverData`.
    Recover {
        /// The pooled-row index.
        row: usize,
    },
}

const MONO: egui::FontId = egui::FontId::monospace(12.0);

fn sect(ui: &mut egui::Ui, name: &str) {
    ui.label(
        egui::RichText::new(format!("── {name} ──────────────────────────────"))
            .font(MONO)
            .color(egui::Color32::from_gray(115)),
    );
}

/// The always-visible program readout — funds + science + active
/// contracts, top-right, drawn in every mode so the program state is
/// never hidden behind a view switch (§3.7's global-channel rule).
/// Interactable: a click opens nothing (the Program button does) — the
/// strip is a readout only.
pub fn program_status(ctx: &egui::Context, funds: f64, science: f64, contracts: usize) {
    let rect = ctx.content_rect();
    egui::Area::new(egui::Id::unique("program_status"))
        .order(egui::Order::Foreground)
        .interactable(false)
        .fixed_pos(egui::pos2(rect.right() - 12.0, rect.top() + 4.0))
        .pivot(egui::Align2::RIGHT_TOP)
        .show(ctx, |ui| {
            ui.label(
                egui::RichText::new(format!("₣ {funds:.0}   ⚛ {science:.1}   ✉ {contracts}"))
                    .font(egui::FontId::monospace(13.0))
                    .color(egui::Color32::from_rgb(160, 220, 160)),
            );
        });
}

/// The Program window — overview, contracts, research, vessel science,
/// reliability. `open` mirrors the close button into the session flag.
/// Emits [`ProgramCommand`]s; `csp-view` resolves them into
/// `SimCommand`s on the outbox. Returns the window rect so the host
/// masks VAB pointer input against it (C14-F2).
#[allow(clippy::too_many_lines)] // one window = one pass
pub fn program_panel(
    ctx: &egui::Context,
    input: &ProgramInput,
    open: &mut bool,
    out: &mut Vec<ProgramCommand>,
) -> egui::Rect {
    let mut o = *open;
    let dim = egui::Color32::from_gray(160);
    let rect = egui::Window::new("Program")
        .open(&mut o)
        .anchor(egui::Align2::LEFT_TOP, egui::vec2(12.0, 34.0))
        .default_size([420.0, 560.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                // Overview — the program ledger the player otherwise
                // never sees (funds/science were sim-only mirrors).
                sect(ui, "OVERVIEW");
                ui.label(
                    egui::RichText::new(format!(
                        "funds ₣ {:.0}   science ⚛ {:.1} (banked {:.1})",
                        input.funds, input.science, input.banked
                    ))
                    .font(MONO)
                    .color(egui::Color32::from_gray(220)),
                );

                // Contracts — offers are acceptable from here;
                // acceptance rides `AcceptContract`.
                sect(ui, "CONTRACTS");
                if input.active.is_empty() && input.offers.is_empty() {
                    ui.label(egui::RichText::new("no contracts on the board").color(dim));
                }
                for a in &input.active {
                    ui.label(egui::RichText::new(format!("▸ {a}")).font(MONO).color(dim));
                }
                for offer in &input.offers {
                    ui.horizontal(|ui| {
                        if ui.small_button("accept").clicked() {
                            out.push(ProgramCommand::AcceptContract { offer: offer.index });
                        }
                        ui.label(egui::RichText::new(&offer.label).font(MONO).color(dim));
                    });
                }

                // Research — the spend side of the loop.
                sect(ui, "RESEARCH");
                match &input.tech {
                    None => {
                        ui.label(egui::RichText::new("no tech tree installed").color(dim));
                    }
                    Some(nodes) => {
                        for n in nodes {
                            let (mark, col) = if n.researched {
                                ("✓", egui::Color32::from_rgb(140, 220, 140))
                            } else {
                                ("○", egui::Color32::from_gray(200))
                            };
                            ui.horizontal(|ui| {
                                ui.label(egui::RichText::new(mark).color(col));
                                ui.label(
                                    egui::RichText::new(format!("{:<28} {:>5.0}", n.name, n.cost))
                                        .font(MONO)
                                        .color(col),
                                );
                                if !n.researched {
                                    let can = n.prereqs_met && n.affordable;
                                    if ui
                                        .add_enabled(can, egui::Button::new("research").small())
                                        .clicked()
                                    {
                                        out.push(ProgramCommand::ResearchTech {
                                            node: n.id.clone(),
                                        });
                                    }
                                    if !n.prereqs_met {
                                        ui.label(egui::RichText::new("prereqs").color(dim).small());
                                    } else if !n.affordable {
                                        ui.label(egui::RichText::new("need ⚛").color(dim).small());
                                    }
                                }
                            });
                        }
                    }
                }
                if !input.tracks.is_empty() {
                    ui.add_space(4.0);
                    for t in &input.tracks {
                        ui.horizontal(|ui| {
                            ui.label(
                                egui::RichText::new(format!(
                                    "{:<28} {}/{}",
                                    t.name, t.level, t.levels
                                ))
                                .font(MONO)
                                .color(egui::Color32::from_gray(200)),
                            );
                            match t.next_cost {
                                Some(cost) => {
                                    let can = t.affordable && t.level < t.cap;
                                    if ui
                                        .add_enabled(
                                            can,
                                            egui::Button::new(format!("+{cost:.0}")).small(),
                                        )
                                        .clicked()
                                    {
                                        out.push(ProgramCommand::ResearchTrack {
                                            track: t.id.clone(),
                                        });
                                    }
                                    if t.level >= t.cap {
                                        ui.label(egui::RichText::new("capped").color(dim).small());
                                    }
                                }
                                None => {
                                    ui.label(
                                        egui::RichText::new(if t.cap == 0 {
                                            "locked"
                                        } else {
                                            "maxed"
                                        })
                                        .color(dim)
                                        .small(),
                                    );
                                }
                            }
                        });
                    }
                }

                // Vessel science — run instruments, bank data.
                sect(ui, "SCIENCE");
                match &input.context {
                    None => {
                        ui.label(egui::RichText::new("no vessel in focus").color(dim));
                    }
                    Some((vessel, situation, biome, body)) => {
                        ui.label(
                            egui::RichText::new(format!(
                                "{vessel} · {situation}{} @ {body}",
                                if biome.is_empty() {
                                    String::new()
                                } else {
                                    format!(" · {biome}")
                                }
                            ))
                            .font(MONO)
                            .color(dim),
                        );
                    }
                }
                for e in &input.experiments {
                    ui.horizontal(|ui| {
                        let status = if e.spent {
                            "spent".to_string()
                        } else {
                            format!("{:.0} data", e.data_units)
                        };
                        ui.label(
                            egui::RichText::new(format!(
                                "{} · {} ({})",
                                e.part, e.experiment, status
                            ))
                            .font(MONO)
                            .color(egui::Color32::from_gray(200)),
                        );
                        if ui
                            .add_enabled(e.runnable, egui::Button::new("run").small())
                            .on_hover_text(if e.runnable {
                                format!("collect {:.0} data units", e.data_units)
                            } else {
                                e.blocked.clone()
                            })
                            .clicked()
                        {
                            out.push(ProgramCommand::RunExperiment {
                                node: e.node,
                                experiment: e.experiment.clone(),
                            });
                        }
                    });
                }
                if input.experiments.is_empty() && input.context.is_some() {
                    ui.label(egui::RichText::new("no experiment modules aboard").color(dim));
                }
                for d in &input.pooled {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("{} · {:.1} data", d.label, d.units))
                                .font(MONO)
                                .color(egui::Color32::from_gray(200)),
                        );
                        if ui
                            .small_button(format!("transmit +{:.1}⚛", d.transmit_yield))
                            .clicked()
                        {
                            out.push(ProgramCommand::Transmit { row: d.index });
                        }
                        if ui
                            .add_enabled(
                                input.latched,
                                egui::Button::new(format!("recover +{:.1}⚛", d.recover_yield))
                                    .small(),
                            )
                            .on_hover_text(if input.latched {
                                "bank and close this record".to_string()
                            } else {
                                "vessel must be on the surface".to_string()
                            })
                            .clicked()
                        {
                            out.push(ProgramCommand::Recover { row: d.index });
                        }
                        if d.transmitted {
                            ui.label(egui::RichText::new("tx'd").color(dim).small());
                        }
                    });
                }

                // Reliability — failures actually fire in the sim
                // (dormant hazard, ignition misfires, Λ crossings);
                // this is where the player sees them.
                if !input.reliability.is_empty() {
                    sect(ui, "RELIABILITY");
                    for r in &input.reliability {
                        let (text, col) = if !r.failures.is_empty() {
                            (
                                format!("{} — FAILED: {}", r.part, r.failures.join(", ")),
                                egui::Color32::from_rgb(235, 120, 90),
                            )
                        } else if r.misfire {
                            (
                                format!("{} — ignition misfire", r.part),
                                egui::Color32::from_rgb(235, 180, 80),
                            )
                        } else if r.thrust_factor < 1.0 {
                            (
                                format!("{} — thrust degraded", r.part),
                                egui::Color32::from_rgb(235, 180, 80),
                            )
                        } else {
                            let w = r.wear.unwrap_or(0.0) * 100.0;
                            (
                                format!(
                                    "{} — ok (wear {:.0}%{})",
                                    r.part,
                                    w,
                                    if r.dormant { ", dormant" } else { "" }
                                ),
                                egui::Color32::from_gray(160),
                            )
                        };
                        ui.label(egui::RichText::new(text).font(MONO).color(col));
                    }
                }
                if !input.notices.is_empty() {
                    sect(ui, "LOG");
                    for n in &input.notices {
                        ui.label(
                            egui::RichText::new(format!("t{} {}", n.tick, n.text))
                                .font(MONO)
                                .color(dim),
                        );
                    }
                }
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING);
    *open = o;
    rect
}
