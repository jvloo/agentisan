use agentisan::{
    fixture,
    model::*,
    registry::Registry,
    server,
    teams::{self, Action, MemberConfig, Provider, TeamConfig},
};

#[tokio::test]
async fn largest_semantic_result_survives_json_escaping_and_http_framing() {
    let temp = tempfile::tempdir().unwrap();
    let data = temp.path().join("state");
    let registry = Registry::open(&fixture::database_path(&data).unwrap())
        .await
        .unwrap();
    let config = TeamConfig {
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
    };
    teams::create(&registry, &data, &config).await.unwrap();
    let token = fixture::read_credential(&data.join("managed/t/lead.token")).unwrap();
    let run = teams::start(&registry, "t", "Work", 4, 4, 60, 10)
        .await
        .unwrap();
    teams::next(&registry).await.unwrap().unwrap();
    teams::act(
        &registry,
        &token,
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
        .bearer_auth(&token)
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
