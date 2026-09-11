use agentisan::{
    fixture,
    model::*,
    registry::Registry,
    server,
    teams::{self, Action, MemberConfig, Provider, TeamConfig},
};
use sqlx::{Connection, SqliteConnection, sqlite::SqliteConnectOptions};
use std::time::Duration;

fn config() -> TeamConfig {
    TeamConfig {
        group: Group {
            id: "g".into(),
            name: "Group".into(),
        },
        team: Team {
            id: "t".into(),
            group_id: "g".into(),
            name: "Team".into(),
        },
        agents: vec![MemberConfig {
            id: "lead".into(),
            name: "Lead".into(),
            role: AgentRole::Lead,
            provider: Provider::Claude,
            executable: std::env::current_exe().unwrap(),
            model: "mock".into(),
            effort: "low".into(),
            instructions: String::new(),
        }],
    }
}

#[tokio::test]
async fn largest_semantic_result_survives_json_escaping_and_http_framing() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let config = config();
    teams::create(&registry, &data, &config).await.unwrap();
    let token = fixture::read_credential(&data.join("managed/t/lead.token")).unwrap();
    let run = teams::start(&registry, "t", "Work", 4, 4, 60, 10)
        .await
        .unwrap();
    let work = teams::next(&registry).await.unwrap().unwrap();
    let lease = fixture::read_credential(&work.credential_file).unwrap();
    teams::act(
        &registry,
        &lease,
        Action::Receive {
            run_id: run.clone(),
        },
    )
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/team-action", listener.local_addr().unwrap());
    let router = server::router(registry.clone());
    let service = tokio::spawn(async { axum::serve(listener, router).await.unwrap() });
    let result = format!("x{}", "\0".repeat(16_383));
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(url)
        .bearer_auth(&lease)
        .json(&Action::Complete {
            run_id: run.clone(),
            result: result.clone(),
        })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(
        teams::inspect_run(&registry, &token, &run).await.unwrap()["result"],
        result
    );
    service.abort();
    registry.close().await;
}

#[tokio::test]
async fn inspection_only_service_fences_preexisting_active_turns() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    teams::create(&registry, &data, &config()).await.unwrap();
    let token = fixture::read_credential(&data.join("managed/t/lead.token")).unwrap();
    let run = teams::start(&registry, "t", "Work", 4, 4, 60, 10)
        .await
        .unwrap();
    let work = teams::next(&registry).await.unwrap().unwrap();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = tokio::spawn(server::serve(registry.clone(), listener, None));
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let inspected = teams::inspect_run(&registry, &token, &run).await.unwrap();
            if inspected["state"] == "interrupted" {
                assert_eq!(inspected["turns"][0]["id"], work.turn_id);
                assert_eq!(inspected["turns"][0]["state"], "unknown");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("inspection-only startup did not fence active work");
    let client = reqwest::Client::builder().no_proxy().build().unwrap();
    let health = client
        .get(format!("http://{address}/health"))
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(health["scheduler"]["state"], "inspection_only");
    let inspected = client
        .post(format!("http://{address}/v1/inspect"))
        .bearer_auth(&token)
        .json(&Query::RunsInspect {
            run_id: run.clone(),
        })
        .send()
        .await
        .unwrap()
        .json::<serde_json::Value>()
        .await
        .unwrap();
    assert_eq!(
        inspected["activity"]["scheduler"]["state"],
        "inspection_only"
    );
    assert_eq!(inspected["activity"]["authoritative"], true);
    service.abort();
    let _ = service.await;
    registry.close().await;
}

#[tokio::test]
async fn scheduler_database_failure_stops_service_and_fences_work() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    teams::create(&registry, &data, &config()).await.unwrap();
    let run = teams::start(&registry, "t", "Work", 4, 4, 60, 10)
        .await
        .unwrap();
    let work = teams::next(&registry).await.unwrap().unwrap();
    // `recover_interrupted` can run, but the scheduler's first query cannot.
    // This models a damaged runtime rather than a model-worker failure.
    registry.close().await;
    let database = fixture::database_path(&data).unwrap();
    let options = SqliteConnectOptions::new()
        .filename(&database)
        .foreign_keys(true);
    let mut damaged = SqliteConnection::connect_with(&options).await.unwrap();
    sqlx::query("ALTER TABLE messages RENAME TO messages_broken")
        .execute(&mut damaged)
        .await
        .unwrap();
    damaged.close().await.unwrap();
    let registry = Registry::open(&database).await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        server::serve(registry.clone(), listener, Some(data.clone())),
    )
    .await
    .expect("scheduler failure left service running")
    .expect_err("scheduler failure must fail the service");
    assert!(result.to_string().contains("worker scheduler stopped"));
    assert!(tokio::net::TcpStream::connect(address).await.is_err());
    let options = SqliteConnectOptions::new()
        .filename(&database)
        .foreign_keys(true);
    let mut checked = SqliteConnection::connect_with(&options).await.unwrap();
    let run_state: String = sqlx::query_scalar("SELECT state FROM runs WHERE id=?")
        .bind(&run)
        .fetch_one(&mut checked)
        .await
        .unwrap();
    let turn_state: String = sqlx::query_scalar("SELECT state FROM turns WHERE id=?")
        .bind(&work.turn_id)
        .fetch_one(&mut checked)
        .await
        .unwrap();
    assert_eq!(run_state, "interrupted");
    assert_eq!(turn_state, "unknown");
    checked.close().await.unwrap();
    registry.close().await;
}

#[tokio::test]
async fn team_action_returns_closed_error_codes() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    teams::create(&registry, &data, &config()).await.unwrap();
    let token = fixture::read_credential(&data.join("managed/t/lead.token")).unwrap();
    let run = teams::start(&registry, "t", "Work", 4, 4, 60, 10)
        .await
        .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1/team-action", listener.local_addr().unwrap());
    let service = tokio::spawn(async move {
        axum::serve(listener, server::router(registry.clone()))
            .await
            .unwrap()
    });
    let response = reqwest::Client::builder()
        .no_proxy()
        .build()
        .unwrap()
        .post(url)
        .bearer_auth(&token)
        .json(&Action::Receive { run_id: run })
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::FORBIDDEN);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["error"],
        "lease_not_active"
    );
    service.abort();
}
