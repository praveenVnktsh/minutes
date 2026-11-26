//! Tauri commands for the people directory and meeting attendees.

use super::{MeetingContext, Person, PersonEdit};
use crate::state::AppState;

#[tauri::command]
pub async fn people_list(state: tauri::State<'_, AppState>) -> Result<Vec<Person>, String> {
    super::list_people(state.db_manager.pool()).await
}

#[tauri::command]
pub async fn people_create(
    state: tauri::State<'_, AppState>,
    edit: PersonEdit,
) -> Result<Person, String> {
    super::create_manual_person(state.db_manager.pool(), &edit).await
}

#[tauri::command]
pub async fn people_update(
    state: tauri::State<'_, AppState>,
    person_id: String,
    edit: PersonEdit,
) -> Result<Person, String> {
    super::update_person(state.db_manager.pool(), &person_id, &edit).await
}

#[tauri::command]
pub async fn people_delete(
    state: tauri::State<'_, AppState>,
    person_id: String,
) -> Result<(), String> {
    super::delete_person(state.db_manager.pool(), &person_id).await
}

#[tauri::command]
pub async fn meeting_attendees_get(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<Vec<Person>, String> {
    super::meeting_attendees(state.db_manager.pool(), &meeting_id).await
}

#[tauri::command]
pub async fn meeting_attendees_add(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    person_id: String,
) -> Result<Vec<Person>, String> {
    let pool = state.db_manager.pool();
    super::add_meeting_attendee(pool, &meeting_id, &person_id, super::SOURCE_MANUAL).await?;
    super::meeting_attendees(pool, &meeting_id).await
}

#[tauri::command]
pub async fn meeting_attendees_remove(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
    person_id: String,
) -> Result<Vec<Person>, String> {
    let pool = state.db_manager.pool();
    super::remove_meeting_attendee(pool, &meeting_id, &person_id).await?;
    super::meeting_attendees(pool, &meeting_id).await
}

#[tauri::command]
pub async fn meeting_people_context(
    state: tauri::State<'_, AppState>,
    meeting_id: String,
) -> Result<MeetingContext, String> {
    super::meeting_context(state.db_manager.pool(), &meeting_id).await
}
