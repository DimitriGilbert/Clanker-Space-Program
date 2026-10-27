//! The Game menu — save slots, load, revert-to-launch, new game.
//!
//! Session-state plumbing like `settings`: `csp-view` owns the
//! [`GameMenuState`]-shaped resource, `csp-app` owns the file IO.
//! The revert path carries the PLAN §3.4 disclosure ("revert is not
//! a reroll — the pre-drawn hazard thresholds survive") at the
//! confirm prompt, exactly as the plan's revert policy mandates.

/// One save slot's listing row — name + the CSPSAVE header's
/// recorded tick and label.
#[derive(Clone, Debug)]
pub struct SaveSlotRow {
    /// Slot name (file stem).
    pub name: String,
    /// The save's recorded sim tick (0 on headers written before the
    /// `tick` envelope field existed).
    pub tick: u64,
    /// The save slot's display label.
    pub label: Option<String>,
}

/// What the menu needs from the world this frame.
pub struct GameMenuInput<'a> {
    /// The save slots on disk, newest first is the app's concern —
    /// rendered in the order given.
    pub saves: &'a [SaveSlotRow],
    /// The revert point's sim tick — `None` disables the Revert
    /// button (no launch has happened yet).
    pub revert_tick: Option<u64>,
}

/// Session state the menu mutates — carried inside `csp-view`'s
/// GameMenuRes.
#[derive(Default)]
pub struct GameMenuState {
    /// The "save as" text field's content.
    pub save_name: String,
    /// The revert disclosure confirm is armed.
    pub confirm_revert: bool,
    /// The new-game confirm is armed.
    pub confirm_new: bool,
}

/// The verbs the menu emitted — drained through `UiOutbox` like the
/// other panel requests (the §2.5 one-frame hand-off).
#[derive(Default)]
pub struct GameMenuOutput {
    /// Save the world to this slot.
    pub save: Option<String>,
    /// Load this slot's world.
    pub load: Option<String>,
    /// Revert-to-launch (post-disclosure).
    pub revert: bool,
    /// New game (post-confirm).
    pub new_game: bool,
}

/// The menu-side mirror of the app's `safe_slot_name` rule (the
/// authoritative check stays in `csp-app` as defense in depth — this
/// one exists so a refused name never reaches it: the button disables
/// and the field keeps the typed text, C22-F6). Keep the two in
/// lockstep: same char class, same length cap, same trim/dot rules.
fn slot_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ' '))
        && name.trim() == name
        && !name.starts_with('.')
}

/// Draw the menu's own bar button + its windows — returns the rects
/// drawn so the VAB pointer mask can exclude them.
#[allow(clippy::too_many_lines)] // one menu pass — the windows are the list
pub fn game_menu(
    ctx: &egui::Context,
    state: &mut GameMenuState,
    input: &GameMenuInput,
    out: &mut GameMenuOutput,
) -> Vec<egui::Rect> {
    let mut rects = Vec::with_capacity(3);
    // No title bar ⇒ drag-anywhere: the strip parks top-right by
    // default but the player can pull it elsewhere like any panel.
    let screen = ctx.content_rect();
    let rect = egui::Window::new("game_menu")
        .title_bar(false)
        .default_pos(egui::pos2(screen.right() - 100.0, screen.top()))
        .resizable(false)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.menu_button("Game", |ui| {
                ui.label("Save");
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut state.save_name)
                            .desired_width(110.0)
                            .hint_text("slot name"),
                    );
                    // Gate on the slot-name rule BEFORE emitting — the
                    // app keeps its own check (defense in depth), but
                    // clearing the field ahead of that validation meant
                    // a refused name ate the typed text (C22-F6). A name
                    // the rule refuses never leaves the field; the
                    // player edits it in place instead of retyping.
                    let name = state.save_name.trim();
                    let save = ui.add_enabled(slot_name_ok(name), egui::Button::new("Save"));
                    // `on_disabled_hover_text` consumes the Response —
                    // rebind; a disabled button can never click anyway.
                    let save = if slot_name_ok(name) {
                        save
                    } else {
                        save.on_disabled_hover_text(
                            "letters, digits, space, - _ . only · ≤ 64 chars · no leading '.'",
                        )
                    };
                    if save.clicked() {
                        out.save = Some(name.to_string());
                        state.save_name.clear();
                        ui.close();
                    }
                });
                if ui.button("Quicksave  [F5]").clicked() {
                    out.save = Some("quicksave".to_string());
                    ui.close();
                }
                ui.separator();
                ui.label("Load");
                if input.saves.is_empty() {
                    ui.weak("(no saves)");
                } else {
                    for slot in input.saves {
                        let label = match &slot.label {
                            Some(l) => format!("{} — {} (tick {})", slot.name, l, slot.tick),
                            None => format!("{} (tick {})", slot.name, slot.tick),
                        };
                        if ui.button(label).clicked() {
                            out.load = Some(slot.name.clone());
                            ui.close();
                        }
                    }
                }
                ui.separator();
                let revert = ui.add_enabled(
                    input.revert_tick.is_some(),
                    egui::Button::new("Revert to launch  [F10]"),
                );
                if revert.clicked() {
                    state.confirm_revert = true;
                    ui.close();
                }
                if input.revert_tick.is_none() {
                    revert.on_disabled_hover_text("no launch yet — nothing to revert to");
                }
                if ui.button("New game").clicked() {
                    state.confirm_new = true;
                    ui.close();
                }
            });
        })
        .map(|r| r.response.rect)
        .unwrap_or(egui::Rect::NOTHING);
    rects.push(rect);

    // PLAN:1180's mandated disclosure: revert keeps the pre-drawn
    // reliability thresholds — the same parts fail at the same Λ.
    // It is a retry of the flying, not a reroll; the prompt must say
    // so or players report it as a bug.
    if state.confirm_revert {
        let rect = egui::Window::new("revert_confirm")
            .title_bar(false)
            .default_pos(egui::pos2(
                screen.center().x - 190.0,
                screen.center().y - 80.0,
            ))
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.heading("REVERT TO LAUNCH?");
                if let Some(tick) = input.revert_tick {
                    ui.label(format!("restores the pre-launch snapshot (tick {tick})"));
                }
                ui.label(
                    "Reliability thresholds are pre-drawn: the same parts will \
                     still fail at the same accumulated hazard.\n\
                     Revert is not a reroll — it is a retry of the flying.",
                );
                ui.horizontal(|ui| {
                    if ui.button("REVERT").clicked() {
                        out.revert = true;
                        state.confirm_revert = false;
                    }
                    if ui.button("Cancel").clicked() {
                        state.confirm_revert = false;
                    }
                });
            })
            .map(|r| r.response.rect)
            .unwrap_or(egui::Rect::NOTHING);
        rects.push(rect);
    }
    if state.confirm_new {
        let rect = egui::Window::new("new_game_confirm")
            .title_bar(false)
            .default_pos(egui::pos2(
                screen.center().x - 170.0,
                screen.center().y - 60.0,
            ))
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.heading("NEW GAME?");
                ui.label("abandons the current session — save first if it matters");
                ui.horizontal(|ui| {
                    if ui.button("NEW GAME").clicked() {
                        out.new_game = true;
                        state.confirm_new = false;
                    }
                    if ui.button("Cancel").clicked() {
                        state.confirm_new = false;
                    }
                });
            })
            .map(|r| r.response.rect)
            .unwrap_or(egui::Rect::NOTHING);
        rects.push(rect);
    }
    rects
}

#[cfg(test)]
mod tests {
    /// C22-F6: the menu-side gate mirrors the app's `safe_slot_name`
    /// — every refusal class stays typed-in-field instead of being
    /// emitted and eaten by the app-side check.
    #[test]
    fn slot_name_rule_mirrors_the_app_gate() {
        use super::slot_name_ok;
        assert!(slot_name_ok("apollo-11"));
        assert!(slot_name_ok("a b_c.d 1"));
        assert!(!slot_name_ok(""));
        assert!(!slot_name_ok("a/b"));
        assert!(!slot_name_ok("a\\b"));
        assert!(!slot_name_ok(".hidden"));
        assert!(!slot_name_ok("trailing "));
        assert!(!slot_name_ok(" leading"));
        assert!(!slot_name_ok("ünïcode"));
        assert!(!slot_name_ok("ctrl\nchar"));
        assert!(!slot_name_ok(&"x".repeat(65)));
        assert!(slot_name_ok(&"x".repeat(64)));
    }
}
