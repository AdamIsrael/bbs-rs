//! End-to-end check of door launching over the web transport: configure a
//! shell "door", drive the menu over a real WebSocket, launch it, and confirm
//! the program ran on a PTY (isatty), saw its environment + drop file, and its
//! output was bridged back to the client.

#![cfg(unix)]

use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;

use arc_swap::ArcSwap;
use futures_util::{SinkExt, StreamExt};
use sqlx::sqlite::SqlitePoolOptions;
use tokio_tungstenite::tungstenite::Message;

use bbs_rs::config::{Door, Settings};
use bbs_rs::services::presence::Presence;
use bbs_rs::web;

#[tokio::test]
async fn door_launches_on_a_pty_over_websocket() {
    // A door script: full-screen clear, print isatty + env + a drop-file line,
    // then wait for one keypress and exit.
    let dir = std::env::temp_dir().join(format!("bbs_door_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("door.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\n\
         printf 'DOOR-DEMO\\r\\n'\n\
         printf 'isatty=%s\\r\\n' \"$( [ -t 0 ] && echo YES || echo no )\"\n\
         printf 'user=%s node=%s\\r\\n' \"$BBS_USER\" \"$BBS_NODE\"\n\
         [ -f DORINFO1.DEF ] && printf 'drop=%s\\r\\n' \"$(sed -n 7p DORINFO1.DEF)\"\n\
         head -c 1 >/dev/null\n\
         printf 'DOOR-BYE\\r\\n'\n",
    )
    .unwrap();

    let db = dir.join("bbs.db");
    let pool = SqlitePoolOptions::new()
        .max_connections(5)
        .connect(&format!("sqlite://{}?mode=rwc", db.display()))
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    bbs_rs::services::seed(&pool, &Default::default())
        .await
        .unwrap();

    // Point cwd at a directory that does NOT exist yet: the door runner must
    // create it before writing the drop file / spawning (regression for a
    // missing `cwd` failing the launch).
    let workdir = dir.join("work_autocreate");
    let settings = Settings {
        doors: vec![Door {
            name: "Demo".into(),
            command: "/bin/sh".into(),
            args: vec![script.to_string_lossy().into_owned()],
            cwd: Some(workdir),
            time_limit_secs: 30,
            drop_file: Some("dorinfo1.def".into()),
        }],
        ..Default::default()
    };
    let state = web::WebState::new(
        pool,
        Arc::new(ArcSwap::from_pointee(settings)),
        Presence::new(),
        Arc::new(AtomicUsize::new(0)),
    );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = web::serve(listener, state).await;
    });

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}/ws"))
        .await
        .unwrap();
    ws.send(Message::Text(r#"{"user":"guest","pass":"guest"}"#.into()))
        .await
        .unwrap();
    ws.send(Message::Text(
        r#"{"type":"resize","cols":80,"rows":24}"#.into(),
    ))
    .await
    .unwrap();

    // Drive the menu: 7× Down reaches "Door Games", Enter opens the door list,
    // Enter launches the first door. Keys are sent as raw terminal bytes (the
    // transport decodes them exactly like a real client).
    tokio::time::sleep(Duration::from_millis(200)).await;
    for _ in 0..7 {
        ws.send(Message::Binary(b"\x1b[B".to_vec().into()))
            .await
            .unwrap();
    }
    ws.send(Message::Binary(b"\r".to_vec().into()))
        .await
        .unwrap(); // open Doors screen
    tokio::time::sleep(Duration::from_millis(150)).await;
    ws.send(Message::Binary(b"\r".to_vec().into()))
        .await
        .unwrap(); // launch door[0]

    // Collect output until the door's markers appear (or time out).
    let mut out = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(8);
    let mut sent_key = false;
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => {
                out.push_str(&String::from_utf8_lossy(&b));
                // Once the door is up and waiting, send Enter so its (cooked-mode)
                // `head -c 1` read completes and the script exits.
                if !sent_key && out.contains("DOOR-DEMO") {
                    sent_key = true;
                    let _ = ws.send(Message::Binary(b"\r".to_vec().into())).await;
                }
                if out.contains("DOOR-BYE") {
                    break;
                }
            }
            Ok(Some(Ok(_))) => {}
            _ => break,
        }
    }

    assert!(out.contains("DOOR-DEMO"), "door did not run; got:\n{out}");
    assert!(
        out.contains("isatty=YES"),
        "door should run on a PTY (isatty); got:\n{out}"
    );
    assert!(
        out.contains("user=guest"),
        "door should see BBS_USER in env; got:\n{out}"
    );
    assert!(
        out.contains("drop=guest"),
        "door should read the drop file; got:\n{out}"
    );
    assert!(
        out.contains("DOOR-BYE"),
        "door should exit after a keypress; got:\n{out}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
