#![cfg(unix)]

use std::time::Duration;

use scell_session::{Encoding, LocalShell, SessionEvent, SessionService};
use scell_term::{RenderSnapshot, TermSize};

fn run_until_exit(shell: LocalShell) -> (Vec<SessionEvent>, String) {
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let (service, events) = SessionService::new(rt.handle().clone());
    let handle = service
        .spawn_local(shell, TermSize::new(40, 5, 8, 16))
        .expect("spawn");

    let mut seen = Vec::new();
    loop {
        let event = events
            .recv_timeout(Duration::from_secs(5))
            .expect("会话应在超时前退出");
        let exited = matches!(event, SessionEvent::Exited { .. });
        seen.push(event);
        if exited {
            break;
        }
    }
    // 退出事件可能早于最后一段输出被读完
    std::thread::sleep(Duration::from_millis(100));
    let mut snap = RenderSnapshot::default();
    handle.terminal().snapshot(&mut snap);
    let text = (0..snap.rows)
        .map(|r| snap.row(r).iter().map(|c| c.ch).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n");
    (seen, text)
}

fn sh(script: &str) -> LocalShell {
    LocalShell {
        program: Some("/bin/sh".into()),
        args: vec!["-c".into(), script.into()],
        ..Default::default()
    }
}

#[test]
fn output_title_and_exit_code() {
    let (events, text) = run_until_exit(sh(r"printf 'hello\033]2;demo\007'; exit 3"));
    assert!(text.contains("hello"), "{text}");
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::Title(_, Some(t)) if t == "demo"))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, SessionEvent::Exited { code: Some(3), .. }))
    );
}

#[test]
fn gbk_output_is_decoded() {
    // “中文”的 GBK 编码
    let mut shell = sh(r"printf '\326\320\316\304'");
    shell.encoding = Encoding::for_label("gbk").expect("gbk");
    let (_, text) = run_until_exit(shell);
    // 宽字符第二格为占位空格
    assert!(text.starts_with("中 文"), "{text}");
}
