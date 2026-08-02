/// Represents the current state of the popup window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupState {
    /// Window is completely hidden.
    Hidden,
    /// Window is in the process of opening and acquiring system focus.
    Opening {
        generation: u64,
        attempts: u32,
        focused_once: bool,
    },
    /// Window is visible and system focus has been confirmed at least once for this generation.
    VisibleAndFocused { generation: u64 },
}

/// Inputs/events received by the popup state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupEvent {
    /// User requested toggle (Super+V global shortcut or tray click).
    ToggleRequested,
    /// System window focus gained (`WindowEvent::Focused(true)`).
    FocusGained,
    /// System window focus lost (`WindowEvent::Focused(false)`).
    FocusLost,
    /// Delayed focus check result (`w.is_focused()`).
    FocusCheckResult { generation: u64, is_focused: bool },
    /// Explicit request to hide window (Escape key, clip copied, explicit command).
    HideRequested,
}

/// Output side-effects produced by the popup state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PopupAction {
    /// Command window: center, show, unminimize, set focus, notify frontend `popup-opened`.
    ShowAndRequestFocus { generation: u64 },
    /// Command window: retry focus (`unminimize`, `set_focus`).
    RetryFocus { generation: u64, attempt: u32 },
    /// Command window: hide.
    HideWindow,
    /// Notify frontend that system focus is confirmed (`popup-focused`).
    NotifyFrontendFocused { generation: u64 },
    /// No window or event action needed.
    NoAction,
}

/// Core state machine for popup lifecycle and focus acquisition logic.
pub struct PopupStateMachine {
    state: PopupState,
    next_generation: u64,
    max_attempts: u32,
}

impl Default for PopupStateMachine {
    fn default() -> Self {
        Self::new(3)
    }
}

impl PopupStateMachine {
    pub fn new(max_attempts: u32) -> Self {
        Self {
            state: PopupState::Hidden,
            next_generation: 1,
            max_attempts,
        }
    }

    pub fn state(&self) -> &PopupState {
        &self.state
    }

    pub fn is_visible(&self) -> bool {
        !matches!(self.state, PopupState::Hidden)
    }

    pub fn handle_event(&mut self, event: PopupEvent) -> PopupAction {
        match (&self.state, event) {
            // ToggleRequested: hidden -> start opening
            (PopupState::Hidden, PopupEvent::ToggleRequested) => {
                let gen = self.next_generation;
                self.next_generation += 1;
                self.state = PopupState::Opening {
                    generation: gen,
                    attempts: 1,
                    focused_once: false,
                };
                PopupAction::ShowAndRequestFocus { generation: gen }
            }
            // ToggleRequested: visible/opening -> hide
            (
                PopupState::Opening { .. } | PopupState::VisibleAndFocused { .. },
                PopupEvent::ToggleRequested,
            ) => {
                self.state = PopupState::Hidden;
                PopupAction::HideWindow
            }

            // FocusGained while Opening
            (
                PopupState::Opening {
                    generation,
                    focused_once,
                    ..
                },
                PopupEvent::FocusGained,
            ) => {
                let gen = *generation;
                let is_first = !*focused_once;
                self.state = PopupState::VisibleAndFocused { generation: gen };
                if is_first {
                    PopupAction::NotifyFrontendFocused { generation: gen }
                } else {
                    PopupAction::NoAction
                }
            }
            // FocusGained while already VisibleAndFocused -> duplicate event, ignore
            (PopupState::VisibleAndFocused { .. }, PopupEvent::FocusGained) => {
                PopupAction::NoAction
            }

            // FocusLost while Opening
            (PopupState::Opening { focused_once, .. }, PopupEvent::FocusLost) => {
                if *focused_once {
                    self.state = PopupState::Hidden;
                    PopupAction::HideWindow
                } else {
                    // Ignore intermediate blur before initial system focus is confirmed
                    PopupAction::NoAction
                }
            }
            // FocusLost while VisibleAndFocused -> hide window
            (PopupState::VisibleAndFocused { .. }, PopupEvent::FocusLost) => {
                self.state = PopupState::Hidden;
                PopupAction::HideWindow
            }

            // FocusCheckResult
            (
                PopupState::Opening {
                    generation,
                    attempts,
                    focused_once,
                },
                PopupEvent::FocusCheckResult {
                    generation: gen_check,
                    is_focused,
                },
            ) => {
                let cur_gen = *generation;
                let cur_att = *attempts;
                let cur_focused_once = *focused_once;

                if gen_check != cur_gen {
                    // Stale event from prior opening cycle
                    return PopupAction::NoAction;
                }

                if is_focused {
                    self.state = PopupState::VisibleAndFocused {
                        generation: cur_gen,
                    };
                    if !cur_focused_once {
                        PopupAction::NotifyFrontendFocused {
                            generation: cur_gen,
                        }
                    } else {
                        PopupAction::NoAction
                    }
                } else if cur_att < self.max_attempts {
                    let next_att = cur_att + 1;
                    self.state = PopupState::Opening {
                        generation: cur_gen,
                        attempts: next_att,
                        focused_once: cur_focused_once,
                    };
                    PopupAction::RetryFocus {
                        generation: cur_gen,
                        attempt: next_att,
                    }
                } else {
                    // Reached max attempts; stop retrying
                    PopupAction::NoAction
                }
            }

            // HideRequested
            (_, PopupEvent::HideRequested) => {
                let was_hidden = matches!(self.state, PopupState::Hidden);
                self.state = PopupState::Hidden;
                if was_hidden {
                    PopupAction::NoAction
                } else {
                    PopupAction::HideWindow
                }
            }

            // Catch-all: ignore unexpected or stale events in any state
            _ => PopupAction::NoAction,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hidden_super_v_starts_opening() {
        let mut sm = PopupStateMachine::new(3);
        assert_eq!(sm.state(), &PopupState::Hidden);

        let action = sm.handle_event(PopupEvent::ToggleRequested);
        assert_eq!(action, PopupAction::ShowAndRequestFocus { generation: 1 });
        assert_eq!(
            sm.state(),
            &PopupState::Opening {
                generation: 1,
                attempts: 1,
                focused_once: false,
            }
        );
    }

    #[test]
    fn test_intermediate_focus_lost_during_opening_is_ignored() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested);

        // Intermediate blur before focus_gained or confirmed focus check
        let action = sm.handle_event(PopupEvent::FocusLost);
        assert_eq!(action, PopupAction::NoAction);
        assert_eq!(
            sm.state(),
            &PopupState::Opening {
                generation: 1,
                attempts: 1,
                focused_once: false,
            }
        );
        assert!(sm.is_visible());
    }

    #[test]
    fn test_focus_gained_confirms_activation() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested);

        let action = sm.handle_event(PopupEvent::FocusGained);
        assert_eq!(action, PopupAction::NotifyFrontendFocused { generation: 1 });
        assert_eq!(sm.state(), &PopupState::VisibleAndFocused { generation: 1 });
    }

    #[test]
    fn test_focus_lost_after_confirmed_focus_hides_window() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested);
        sm.handle_event(PopupEvent::FocusGained);

        let action = sm.handle_event(PopupEvent::FocusLost);
        assert_eq!(action, PopupAction::HideWindow);
        assert_eq!(sm.state(), &PopupState::Hidden);
    }

    #[test]
    fn test_repeat_super_v_while_visible_hides_window() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested);
        assert!(sm.is_visible());

        let action = sm.handle_event(PopupEvent::ToggleRequested);
        assert_eq!(action, PopupAction::HideWindow);
        assert_eq!(sm.state(), &PopupState::Hidden);
    }

    #[test]
    fn test_multiple_focus_gained_do_not_duplicate_notification() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested);

        let first = sm.handle_event(PopupEvent::FocusGained);
        assert_eq!(first, PopupAction::NotifyFrontendFocused { generation: 1 });

        let second = sm.handle_event(PopupEvent::FocusGained);
        assert_eq!(second, PopupAction::NoAction);
    }

    #[test]
    fn test_failed_first_focus_check_triggers_retry() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested);

        let action = sm.handle_event(PopupEvent::FocusCheckResult {
            generation: 1,
            is_focused: false,
        });
        assert_eq!(
            action,
            PopupAction::RetryFocus {
                generation: 1,
                attempt: 2,
            }
        );
        assert_eq!(
            sm.state(),
            &PopupState::Opening {
                generation: 1,
                attempts: 2,
                focused_once: false,
            }
        );
    }

    #[test]
    fn test_max_attempts_reached_prevents_infinite_loop() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested);

        // Attempt 1 -> failed -> Retry attempt 2
        sm.handle_event(PopupEvent::FocusCheckResult {
            generation: 1,
            is_focused: false,
        });
        // Attempt 2 -> failed -> Retry attempt 3
        sm.handle_event(PopupEvent::FocusCheckResult {
            generation: 1,
            is_focused: false,
        });
        // Attempt 3 -> failed -> NoAction (stopped)
        let action = sm.handle_event(PopupEvent::FocusCheckResult {
            generation: 1,
            is_focused: false,
        });
        assert_eq!(action, PopupAction::NoAction);
        assert_eq!(
            sm.state(),
            &PopupState::Opening {
                generation: 1,
                attempts: 3,
                focused_once: false,
            }
        );

        // Subsequent check results still return NoAction
        let action2 = sm.handle_event(PopupEvent::FocusCheckResult {
            generation: 1,
            is_focused: false,
        });
        assert_eq!(action2, PopupAction::NoAction);
    }

    #[test]
    fn test_stale_event_from_previous_generation_is_ignored() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested); // gen 1
        sm.handle_event(PopupEvent::ToggleRequested); // hide -> Hidden
        sm.handle_event(PopupEvent::ToggleRequested); // gen 2

        // Stale check result for gen 1 received during gen 2 opening
        let action = sm.handle_event(PopupEvent::FocusCheckResult {
            generation: 1,
            is_focused: true,
        });
        assert_eq!(action, PopupAction::NoAction);
        assert_eq!(
            sm.state(),
            &PopupState::Opening {
                generation: 2,
                attempts: 1,
                focused_once: false,
            }
        );
    }

    #[test]
    fn test_hide_requested_resets_state_to_hidden() {
        let mut sm = PopupStateMachine::new(3);
        sm.handle_event(PopupEvent::ToggleRequested);
        sm.handle_event(PopupEvent::FocusGained);
        assert_eq!(sm.state(), &PopupState::VisibleAndFocused { generation: 1 });

        let action = sm.handle_event(PopupEvent::HideRequested);
        assert_eq!(action, PopupAction::HideWindow);
        assert_eq!(sm.state(), &PopupState::Hidden);
    }
}
