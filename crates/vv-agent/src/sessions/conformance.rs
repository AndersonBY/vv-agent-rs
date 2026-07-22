use serde_json::Value;

use crate::types::{Message, ToolCall};

use super::{SessionItem, SessionStore};

pub async fn session_store_conformance(store: &dyn SessionStore) -> Result<(), String> {
    let session = store.session("conformance-thread");
    let other_session = store.session("conformance-thread-other");
    session.clear_session().await?;
    other_session.clear().await?;

    let mut user = Message::user("inspect the image");
    user.image_url = Some("data:image/png;base64,AA==".to_string());
    user.metadata.insert("sequence".to_string(), Value::from(1));

    let mut assistant = Message::assistant("");
    assistant.name = Some("planner".to_string());
    assistant.reasoning_content = Some("Check persistence details.".to_string());
    assistant.tool_calls = vec![ToolCall::new(
        "call_1",
        "lookup",
        [(
            "query".to_string(),
            Value::String("session parity".to_string()),
        )]
        .into_iter()
        .collect(),
    )];
    assistant
        .metadata
        .insert("sequence".to_string(), Value::from(2));

    let mut tool = Message::tool("result: ok", "call_1");
    tool.name = Some("lookup".to_string());
    tool.image_url = Some("data:image/png;base64,AQ==".to_string());
    tool.metadata.insert("sequence".to_string(), Value::from(3));

    let expected = [user, assistant, tool]
        .iter()
        .map(|message| {
            SessionItem::from_message(message)
                .ok_or_else(|| "failed to create conformance session item".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    session.add_items(expected.clone()).await?;

    let same_session = store.session("conformance-thread");
    let items = same_session.get_items(None).await?;
    if items != expected {
        return Err("session store did not preserve appended messages".to_string());
    }
    if same_session.get_items(Some(2)).await? != expected[1..] {
        return Err("session store limit did not return newest messages in order".to_string());
    }
    if !same_session.get_items(Some(0)).await?.is_empty() {
        return Err("session store limit=0 must return no messages".to_string());
    }
    let mut isolated = same_session.get_items(None).await?;
    let Some(first) = isolated.first_mut() else {
        return Err("session store returned no snapshot items".to_string());
    };
    match first {
        SessionItem::Message { message } => {
            message.content = "mutated outside the store".to_string();
        }
        SessionItem::User { content }
        | SessionItem::Assistant { content }
        | SessionItem::System { content }
        | SessionItem::Tool { content, .. } => {
            *content = "mutated outside the store".to_string();
        }
    }
    if same_session
        .get_items(None)
        .await?
        .first()
        .map(SessionItem::to_message)
        .map(|message| message.content)
        != Some(expected[0].to_message().content)
    {
        return Err("session store leaked mutable snapshot items".to_string());
    }
    if !other_session.get_items(None).await?.is_empty() {
        return Err("session store did not isolate session ids".to_string());
    }

    let popped = same_session.pop_item().await?;
    if popped.as_ref() != expected.last() {
        return Err("session store pop_item returned an unexpected message".to_string());
    }
    if same_session.get_items(None).await? != expected[..2] {
        return Err("session store pop_item did not remove the message".to_string());
    }

    same_session.clear().await?;
    if !session.get_items(None).await?.is_empty() {
        return Err("session store clear did not clear the session".to_string());
    }
    Ok(())
}
