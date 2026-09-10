use agentisan::model::{AgentId, CredentialHash, Fixture, Group, GroupId, PrincipalId, Query};
use agentisan::registry::{Registry, RegistryError};

fn fixture() -> Fixture {
    serde_json::from_str(include_str!("../examples/registry.json")).expect("fixture parses")
}

fn token_for(principal: &str, token: &str) -> CredentialHash {
    CredentialHash {
        principal_id: PrincipalId(principal.to_owned()),
        sha256: agentisan::registry::credential_hash(token),
    }
}

fn creds(fx: &Fixture) -> Vec<CredentialHash> {
    fx.principals
        .iter()
        .map(|p| token_for(p.id.as_str(), &format!("tok-{}", p.id.as_str())))
        .collect()
}

#[tokio::test]
async fn repeated_registration_and_reopen_persists_identity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.sqlite");
    let fx = fixture();
    let cred_hashes = creds(&fx);

    let reg = Registry::open(&path).await.unwrap();
    reg.register_fixture(&fx, &cred_hashes).await.unwrap();
    reg.register_fixture(&fx, &cred_hashes).await.unwrap();

    let out = reg
        .inspect(Some("tok-inventory_reader"), Query::Whoami {})
        .await
        .unwrap();
    assert_eq!(out["status"], "bound");
    assert_eq!(out["agent_id"], "inventory_lead");
    reg.close().await;

    let reg2 = Registry::open(&path).await.unwrap();
    let out2 = reg2
        .inspect(Some("tok-inventory_reader"), Query::Whoami {})
        .await
        .unwrap();
    assert_eq!(out2["status"], "bound");
    assert_eq!(out2["agent_id"], "inventory_lead");
    reg2.close().await;
}

#[tokio::test]
async fn overlapping_native_session_ids_distinct_namespaces_authorize() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.sqlite");
    let fx = fixture();
    let cred_hashes = creds(&fx);

    let reg = Registry::open(&path).await.unwrap();
    reg.register_fixture(&fx, &cred_hashes).await.unwrap();

    let inv = reg
        .inspect(Some("tok-inventory_reader"), Query::Whoami {})
        .await
        .unwrap();
    assert_eq!(inv["agent_id"], "inventory_lead");

    let sup = reg
        .inspect(Some("tok-support_reader"), Query::Whoami {})
        .await
        .unwrap();
    assert_eq!(sup["agent_id"], "support_lead");

    let obs = reg
        .inspect(Some("tok-observer"), Query::Whoami {})
        .await
        .unwrap();
    assert_eq!(obs["status"], "unbound");

    reg.close().await;
}

#[tokio::test]
async fn changing_native_binding_rejects_and_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.sqlite");
    let fx = fixture();
    let cred_hashes = creds(&fx);

    let reg = Registry::open(&path).await.unwrap();
    reg.register_fixture(&fx, &cred_hashes).await.unwrap();

    let mut mutated = fx.clone();
    let lead = mutated
        .agents
        .iter_mut()
        .find(|a| a.id.as_str() == "inventory_lead")
        .unwrap();
    lead.native_binding.host_id = "changed-host".to_owned();
    mutated.groups.push(Group {
        id: GroupId("newgroup".into()),
        name: "New Group".into(),
    });

    let err = reg
        .register_fixture(&mutated, &cred_hashes)
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::Conflict(_)));

    let groups = reg
        .inspect(Some("tok-inventory_reader"), Query::GroupsList {})
        .await
        .unwrap();
    let names: Vec<&str> = groups["groups"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"New Group"));

    let whoami = reg
        .inspect(Some("tok-inventory_reader"), Query::Whoami {})
        .await
        .unwrap();
    assert_eq!(whoami["agent_id"], "inventory_lead");

    // A new group is invisible to this reader even if a broken import leaked it.
    // Import a different definition under that ID: success proves it was rolled back.
    let mut after_rollback = fixture();
    after_rollback.groups.push(Group {
        id: GroupId("newgroup".into()),
        name: "Different definition after rollback".into(),
    });
    reg.register_fixture(&after_rollback, &cred_hashes)
        .await
        .unwrap();

    reg.close().await;
}

#[tokio::test]
async fn credentials_are_not_replaceable_and_hashes_cannot_authenticate() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::open(&dir.path().join("registry.sqlite"))
        .await
        .unwrap();
    let fx = fixture();
    let mut credentials = creds(&fx);
    registry.register_fixture(&fx, &credentials).await.unwrap();
    assert!(matches!(
        registry
            .inspect(Some(&credentials[0].sha256), Query::Whoami {})
            .await,
        Err(RegistryError::Unauthorized)
    ));
    credentials[0].sha256 = agentisan::registry::credential_hash("replacement");
    assert!(matches!(
        registry.register_fixture(&fx, &credentials).await,
        Err(RegistryError::Conflict(_))
    ));
    assert_eq!(
        registry
            .inspect(Some("tok-inventory_reader"), Query::Whoami {})
            .await
            .unwrap()["status"],
        "bound"
    );
    assert!(matches!(
        registry
            .inspect(Some("replacement"), Query::Whoami {})
            .await,
        Err(RegistryError::Unauthorized)
    ));
    assert!(matches!(
        registry.inspect(None, Query::GroupsList {}).await,
        Err(RegistryError::Unauthorized)
    ));
    registry.close().await;
}

#[tokio::test]
async fn invalid_parent_and_duplicate_native_binding_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("registry.sqlite");
    let base = fixture();
    let cred_hashes = creds(&base);

    let reg = Registry::open(&path).await.unwrap();

    // Child pointing to itself as parent.
    let mut self_parent = base.clone();
    {
        let lead = self_parent
            .agents
            .iter_mut()
            .find(|a| a.id.as_str() == "inventory_lead")
            .unwrap();
        lead.parent_agent_id = Some(AgentId("inventory_lead".into()));
    }
    let err = reg
        .register_fixture(&self_parent, &cred_hashes)
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::Invalid(_)));

    // Child pointing to a parent belonging to another team.
    let mut cross_team = base.clone();
    {
        let worker = cross_team
            .agents
            .iter_mut()
            .find(|a| a.id.as_str() == "inventory_worker")
            .unwrap();
        worker.parent_agent_id = Some(AgentId("support_lead".into()));
    }
    let err = reg
        .register_fixture(&cross_team, &cred_hashes)
        .await
        .unwrap_err();
    assert!(matches!(err, RegistryError::Invalid(_)));

    // Duplicate exact native binding across two distinct agents.
    let mut dup_binding = base.clone();
    {
        let src = dup_binding
            .agents
            .iter()
            .find(|a| a.id.as_str() == "inventory_lead")
            .unwrap()
            .native_binding
            .clone();
        let worker = dup_binding
            .agents
            .iter_mut()
            .find(|a| a.id.as_str() == "inventory_worker")
            .unwrap();
        worker.native_binding = src;
    }
    let err = reg
        .register_fixture(&dup_binding, &cred_hashes)
        .await
        .unwrap_err();
    assert!(matches!(
        err,
        RegistryError::Invalid(_) | RegistryError::Conflict(_)
    ));

    reg.close().await;
}
