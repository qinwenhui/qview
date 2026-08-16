//! Quick test of the tokenizer against a synthetic log line.

use qview_core::cache::DisplayLine;
use qview::tui::tokenize;

fn main() {
    let cases = [
        "2026-07-31 12:34:56.789 [INFO ] auth worker-12 req=abc123 ip=10.0.0.1 dur=2703us status=200 \"hello world\"",
        "2026-07-31T12:34:56Z [ERROR] db a9eab706-619f-4e2a-9c41-b6c5d2e8f910 192.168.1.1 500",
        "2026-07-31 [WARN ] cache miss key=user_session_id val=987",
    ];
    for s in cases {
        let d = DisplayLine {
            text: s.to_string(),
            matches: Vec::new(),
            truncated_right: false,
            truncated_left: false,
            modified: false,
        };
        let spans = tokenize::style_spans(&d);
        println!("INPUT: {}", s);
        for sp in &spans {
            println!("  {:?}  {:?}", sp.style, sp.text);
        }
        println!();
    }
}