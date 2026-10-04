use serde_json::{Value, json};

#[test]
fn exported_contract_is_current_and_covers_registered_operations() {
    let generated = serde_json::to_value(server::openapi()).unwrap();
    let checked_in: Value =
        serde_json::from_str(include_str!("../../../docs/api/openapi.json")).unwrap();
    assert_eq!(generated, checked_in, "run pnpm -C apps/web api:generate");
    let paths = generated["paths"].as_object().unwrap();
    assert_eq!(paths.len(), 9);
    assert_eq!(
        paths["/api/runs/{id}/{action}"]["post"]["operationId"],
        "controlRun"
    );
    assert!(
        paths["/api/chat"]["post"]["responses"]["200"]["content"]
            .get("text/event-stream")
            .is_some()
    );
    let schemas = &generated["components"]["schemas"];
    assert_eq!(
        schemas["RunAction"]["enum"],
        json!(["pause", "resume", "cancel", "steer"])
    );
    assert!(
        schemas["ConversationView"]["properties"]
            .get("history")
            .is_none()
    );
    assert!(schemas["RunView"]["properties"].get("checkpoint").is_none());
}

#[test]
fn host_settings_parse_without_environment_mutation_and_validate_budgets() {
    use server::config::ServerConfig;
    let parse = |pairs: &[(&str, &str)]| {
        envy::from_iter::<_, ServerConfig>(
            pairs
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string())),
        )
    };
    let valid = parse(&[
        ("DATABASE_URL", "postgresql://local/db"),
        ("SERVER_ADDR", "127.0.0.1:3002"),
        ("DB_POOL_SIZE", "12"),
        ("QUEUE_POOL_SIZE", "6"),
    ])
    .unwrap();
    valid.validate().unwrap();
    assert_eq!(valid.server_addr.port(), 3002);
    assert_eq!(valid.pool(valid.db_pool_size).max_connections, 12);
    assert_eq!(valid.pool(valid.queue_pool_size).max_connections, 6);
    assert!(parse(&[("WORKER_CONCURRENCY", "invalid")]).is_err());
    for (key, value) in [
        ("DB_POOL_SIZE", "0"),
        ("QUEUE_POOL_SIZE", "1025"),
        ("DB_POOL_WAIT_SECONDS", "0"),
        ("DB_CONNECT_TIMEOUT_SECONDS", "0"),
        ("RUN_HEARTBEAT_SECONDS", "30"),
        ("CORS_ALLOWED_ORIGINS", "*"),
    ] {
        let invalid = parse(&[("DATABASE_URL", "postgresql://local/db"), (key, value)]).unwrap();
        assert!(invalid.validate().is_err(), "accepted invalid {key}");
    }
}

#[test]
fn model_catalog_validates_choices_and_provider_supported_efforts() {
    use server::api::{ChatConfig, ReasoningEffort};
    let config = ChatConfig::new(
        "bigmodel::glm-5.3-flash",
        "bigmodel::glm-5.3,bigmodel::glm-4.7",
    )
    .unwrap();
    assert_eq!(
        config.resolve(None, None).unwrap(),
        ("bigmodel::glm-5.3-flash".into(), Some("low".into()))
    );
    assert_eq!(
        config
            .resolve(Some("bigmodel::glm-5.3".into()), Some(ReasoningEffort::Max))
            .unwrap()
            .1
            .as_deref(),
        Some("max")
    );
    assert!(config.resolve(Some("unknown".into()), None).is_err());
    assert!(
        config
            .resolve(Some("bigmodel::glm-4.7".into()), Some(ReasoningEffort::Low))
            .is_err()
    );
    assert!(ChatConfig::new("bigmodel::glm-5.3", "openai::gpt-4.1").is_err());
}
