//! Port of upstream/apps/web/src/components/chat/useComposerMultilinePrompt.test.ts

use super::*;

struct FakeBody {
    editor_client_width: f64,
    body_client_width: f64,
    line_height: f64,
    content: (f64, f64),
}

impl PromptBody for FakeBody {
    fn editor_client_width(&self) -> Option<f64> {
        Some(self.editor_client_width)
    }
    fn body_client_width(&self) -> f64 {
        self.body_client_width
    }
    fn body_horizontal_padding(&self) -> (f64, f64) {
        (16.0, 16.0)
    }
    fn editor_line_height(&self) -> f64 {
        self.line_height
    }
    fn content_bounds(&self) -> (f64, f64) {
        self.content
    }
}

struct Input {
    width: f64,
    height: f64,
    resting: bool,
    hidden: bool,
}

fn input(width: f64, height: f64) -> Input {
    Input {
        width,
        height,
        resting: false,
        hidden: false,
    }
}

fn body(input: &Input) -> FakeBody {
    FakeBody {
        editor_client_width: if input.hidden {
            0.0
        } else if input.resting {
            400.0
        } else {
            500.0
        },
        body_client_width: 532.0,
        line_height: if input.resting { 32.0 } else { 22.75 },
        content: (input.width, input.height),
    }
}

fn measure(input: Input) -> Option<bool> {
    measure_composer_multiline_prompt(&body(&input))
}

#[test]
fn allows_a_single_line_even_though_the_editor_has_a_larger_minimum_height() {
    assert_eq!(measure(input(500.0, 22.75)), Some(false));
}

#[test]
fn keeps_soft_wrapped_lines_expanded() {
    assert_eq!(measure(input(500.0, 45.5)), Some(true));
}

#[test]
fn recognizes_a_long_restored_draft_while_the_resting_row_is_unwrapped() {
    let resting = Input {
        resting: true,
        ..input(700.0, 32.0)
    };
    assert_eq!(measure(resting), Some(true));
}

#[test]
fn uses_the_expanded_width_so_inline_actions_cannot_cause_a_collapse_loop() {
    let resting = Input {
        resting: true,
        ..input(450.0, 32.0)
    };
    assert_eq!(measure(resting), Some(false));
    assert_eq!(measure(input(500.0, 22.75)), Some(false));
}

#[test]
fn allows_collapse_again_after_deleting_the_second_line() {
    assert_eq!(measure(input(500.0, 45.5)), Some(true));
    assert_eq!(measure(input(500.0, 22.75)), Some(false));
}

#[test]
fn retains_the_previous_measurement_when_the_editor_is_hidden() {
    let hidden = Input {
        hidden: true,
        ..input(0.0, 0.0)
    };
    assert_eq!(measure(hidden), None);
}

#[test]
fn a_missing_editor_measures_nothing() {
    struct NoEditor;
    impl PromptBody for NoEditor {
        fn editor_client_width(&self) -> Option<f64> {
            None
        }
        fn body_client_width(&self) -> f64 {
            532.0
        }
        fn body_horizontal_padding(&self) -> (f64, f64) {
            (16.0, 16.0)
        }
        fn editor_line_height(&self) -> f64 {
            22.75
        }
        fn content_bounds(&self) -> (f64, f64) {
            (500.0, 45.5)
        }
    }
    assert_eq!(measure_composer_multiline_prompt(&NoEditor), None);
}

#[test]
fn the_hook_state_keeps_its_last_answer_while_hidden() {
    let mut prompt = ComposerMultilinePrompt::default();
    assert!(!prompt.is_multiline());
    assert!(prompt.remeasure(&body(&input(500.0, 45.5))));
    let hidden = Input {
        hidden: true,
        ..input(0.0, 0.0)
    };
    assert!(prompt.remeasure(&body(&hidden)));
    assert!(!prompt.remeasure(&body(&input(500.0, 22.75))));
}
