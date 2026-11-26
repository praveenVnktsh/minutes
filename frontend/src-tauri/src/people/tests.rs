use super::*;
use sqlx::SqlitePool;

async fn pool() -> SqlitePool {
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect("sqlite::memory:")
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::migrate!("./migrations").run(&pool).await.unwrap();
    pool
}

fn linear(id: &str, name: &str, email: &str) -> PersonInput {
    PersonInput {
        source: SOURCE_LINEAR.into(),
        external_id: id.into(),
        name: name.into(),
        email: Some(email.into()),
        team: Some("Platform".into()),
        projects: vec![Project {
            title: "Offline mode".into(),
            url: None,
            source: SOURCE_LINEAR.into(),
        }],
        ..Default::default()
    }
}

async fn meeting(pool: &SqlitePool) -> String {
    crate::database::repositories::meeting::MeetingsRepository::create_meeting(
        pool, "Sync", None, false,
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_resync_updates_the_same_person() {
    let pool = pool().await;
    let first = upsert_from_source(&pool, &linear("u1", "Dev Patel", "Dev@Acme.com"))
        .await
        .unwrap();
    let mut again = linear("u1", "Dev Patel", "dev@acme.com");
    again.team = Some("Infra".into());
    let second = upsert_from_source(&pool, &again).await.unwrap();
    assert_eq!(first, second);
    let people = list_people(&pool).await.unwrap();
    assert_eq!(people.len(), 1);
    assert_eq!(people[0].email.as_deref(), Some("dev@acme.com"));
    assert_eq!(people[0].team.as_deref(), Some("Infra"));
}

#[tokio::test]
async fn sources_merge_by_email_and_keep_each_others_projects() {
    let pool = pool().await;
    let id = upsert_from_source(&pool, &linear("u1", "Dev Patel", "dev@acme.com"))
        .await
        .unwrap();
    let notion = PersonInput {
        source: SOURCE_NOTION.into(),
        external_id: "page-9".into(),
        name: "Dev Patel".into(),
        email: Some("DEV@acme.com".into()),
        role: Some("Engineering lead".into()),
        projects: vec![Project {
            title: "Q3 roadmap".into(),
            url: None,
            source: SOURCE_NOTION.into(),
        }],
        ..Default::default()
    };
    assert_eq!(upsert_from_source(&pool, &notion).await.unwrap(), id);
    let person = get_person(&pool, &id).await.unwrap().unwrap();
    assert_eq!(person.role.as_deref(), Some("Engineering lead"));
    assert_eq!(
        person.team.as_deref(),
        Some("Platform"),
        "an empty field doesn't clear"
    );
    let titles: Vec<&str> = person.projects.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["Offline mode", "Q3 roadmap"]);
    assert_eq!(person.sources.len(), 2);

    // A Linear re-sync replaces only Linear's projects.
    let mut resync = linear("u1", "Dev Patel", "dev@acme.com");
    resync.projects = vec![];
    upsert_from_source(&pool, &resync).await.unwrap();
    let person = get_person(&pool, &id).await.unwrap().unwrap();
    let titles: Vec<&str> = person.projects.iter().map(|p| p.title.as_str()).collect();
    assert_eq!(titles, ["Q3 roadmap"]);
}

#[tokio::test]
async fn manual_edits_survive_a_sync() {
    let pool = pool().await;
    let id = upsert_from_source(&pool, &linear("u1", "Devendra Patel", "dev@acme.com"))
        .await
        .unwrap();
    update_person(
        &pool,
        &id,
        &PersonEdit {
            name: Some("Dev Patel".into()),
            role: Some("Tech lead".into()),
            aliases: Some(vec!["Dev".into()]),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let mut resync = linear("u1", "Devendra Patel", "dev@acme.com");
    resync.role = Some("Engineer".into());
    resync.aliases = vec!["Devendra".into()];
    upsert_from_source(&pool, &resync).await.unwrap();
    let person = get_person(&pool, &id).await.unwrap().unwrap();
    assert_eq!(person.name, "Dev Patel");
    assert_eq!(person.role.as_deref(), Some("Tech lead"));
    assert_eq!(person.aliases, ["Dev"]);
}

#[tokio::test]
async fn only_one_person_is_me_and_emails_stay_unique() {
    let pool = pool().await;
    let a = upsert_from_source(&pool, &linear("u1", "Ana", "ana@acme.com"))
        .await
        .unwrap();
    let b = upsert_from_source(&pool, &linear("u2", "Ben", "ben@acme.com"))
        .await
        .unwrap();
    update_person(
        &pool,
        &a,
        &PersonEdit {
            is_me: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    update_person(
        &pool,
        &b,
        &PersonEdit {
            is_me: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(me(&pool).await.unwrap().unwrap().id, b);
    let clash = update_person(
        &pool,
        &a,
        &PersonEdit {
            email: Some("BEN@acme.com".into()),
            ..Default::default()
        },
    )
    .await;
    assert!(clash.is_err());
}

#[tokio::test]
async fn attendees_and_context() {
    let pool = pool().await;
    let m = meeting(&pool).await;
    let dev = upsert_from_source(&pool, &linear("u1", "Dev Patel", "dev@acme.com"))
        .await
        .unwrap();
    let me_id = create_manual_person(
        &pool,
        &PersonEdit {
            name: Some("Praveen".into()),
            is_me: Some(true),
            ..Default::default()
        },
    )
    .await
    .unwrap()
    .id;
    add_meeting_attendee(&pool, &m, &dev, SOURCE_CALENDAR)
        .await
        .unwrap();
    add_meeting_attendee(&pool, &m, &dev, SOURCE_MANUAL)
        .await
        .unwrap(); // no duplicate
    let context = meeting_context(&pool, &m).await.unwrap();
    assert_eq!(context.attendees.len(), 1);
    assert_eq!(context.me.as_ref().unwrap().id, me_id);
    assert!(context.vocabulary.contains(&"Dev Patel".to_string()));
    assert!(context.vocabulary.contains(&"Offline mode".to_string()));
    assert!(context.vocabulary.contains(&"Praveen".to_string()));
    assert_eq!(resolve_name(&context, "dev").unwrap().id, dev);
    assert!(resolve_name(&context, "Priya").is_none());
    let block = render_context_block(&context, 500);
    assert!(
        block.starts_with("- Dev Patel (Platform). Projects: Offline mode"),
        "{block}"
    );
    assert_eq!(render_context_block(&context, 5), "");

    remove_meeting_attendee(&pool, &m, &dev).await.unwrap();
    assert!(meeting_attendees(&pool, &m).await.unwrap().is_empty());
}

#[tokio::test]
async fn deleting_a_meeting_or_person_drops_attendance() {
    let pool = pool().await;
    let m = meeting(&pool).await;
    let dev = upsert_from_source(&pool, &linear("u1", "Dev Patel", "dev@acme.com"))
        .await
        .unwrap();
    add_meeting_attendee(&pool, &m, &dev, SOURCE_MANUAL)
        .await
        .unwrap();
    crate::database::repositories::meeting::MeetingsRepository::delete_meeting(&pool, &m)
        .await
        .unwrap();
    let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM meeting_attendees")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(left, 0);
    assert!(
        get_person(&pool, &dev).await.unwrap().is_some(),
        "the person outlives the meeting"
    );

    let m2 = meeting(&pool).await;
    add_meeting_attendee(&pool, &m2, &dev, SOURCE_MANUAL)
        .await
        .unwrap();
    delete_person(&pool, &dev).await.unwrap();
    assert!(meeting_attendees(&pool, &m2).await.unwrap().is_empty());
}

#[tokio::test]
async fn forgetting_a_source_removes_people_only_it_knew() {
    let pool = pool().await;
    let only_linear = upsert_from_source(&pool, &linear("u1", "Dev", "dev@acme.com"))
        .await
        .unwrap();
    let edited = upsert_from_source(&pool, &linear("u2", "Ana", "ana@acme.com"))
        .await
        .unwrap();
    update_person(
        &pool,
        &edited,
        &PersonEdit {
            role: Some("PM".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(forget_source(&pool, SOURCE_LINEAR).await.unwrap(), 1);
    assert!(get_person(&pool, &only_linear).await.unwrap().is_none());
    let ana = get_person(&pool, &edited).await.unwrap().unwrap();
    assert!(ana.projects.is_empty(), "Linear's projects go with it");
    assert!(ana.sources.is_empty());
}

#[test]
fn vocabulary_is_deduplicated_and_capped() {
    let person = |name: &str| Person {
        id: name.into(),
        name: name.into(),
        email: None,
        role: None,
        team: Some("Growth".into()),
        aliases: vec![],
        projects: vec![],
        notes: None,
        is_me: false,
        locked_fields: vec![],
        sources: vec![],
    };
    let people: Vec<Person> = (0..300).map(|i| person(&format!("P{i}"))).collect();
    let vocab = vocabulary_for(&people);
    assert_eq!(vocab.len(), MAX_VOCABULARY);
    assert_eq!(vocab.iter().filter(|v| *v == "Growth").count(), 1);
}
