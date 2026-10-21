//! Provider-independent MIME composition. Attachments are private transfer handles,
//! never paths on the installation or files owned by another account.
use super::{connections::Connection, files::Files};
use crate::{
    config::id,
    error::{Error, Result},
    validation::text,
};
use mail_builder::{MessageBuilder, headers::address::Address};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn header(value: &str, limit: usize) -> Result<&str> {
    if value.len() > limit || value.chars().any(char::is_control) {
        return Err(Error::bad("Invalid mail header."));
    }
    Ok(value)
}

fn address(value: &Value) -> Result<Address<'static>> {
    let email = value.as_str().unwrap_or_else(|| text(value, "email"));
    header(email, 254)?;
    let parts = email.split('@').collect::<Vec<_>>();
    if parts.len() != 2
        || parts.iter().any(|p| p.is_empty())
        || !email.is_ascii()
        || email
            .bytes()
            .any(|b| b.is_ascii_whitespace() || b"<>,;:\"\\()[]".contains(&b))
    {
        return Err(Error::bad(
            "Use an email address or {email, name}; recipient lists must be arrays.",
        ));
    }
    let name = value["name"].as_str().map(|s| header(s, 500)).transpose()?;
    Ok(Address::new_address(
        name.map(str::to_owned),
        email.to_owned(),
    ))
}

fn addresses(value: &Value) -> Result<Vec<Address<'static>>> {
    let entries = value
        .as_array()
        .ok_or_else(|| Error::bad("Recipients must be an array."))?;
    if entries.len() > 500 {
        return Err(Error::bad("At most 500 recipients per field."));
    }
    entries.iter().map(address).collect()
}

fn message_ids(value: &Value) -> Result<Vec<String>> {
    let entries = value
        .as_array()
        .ok_or_else(|| Error::bad("Message references must be an array."))?;
    if entries.len() > 100 {
        return Err(Error::bad("At most 100 message references."));
    }
    entries
        .iter()
        .map(|v| {
            let value = v
                .as_str()
                .ok_or_else(|| Error::bad("Invalid message reference."))?
                .trim_matches(['<', '>']);
            header(value, 998)?;
            if value.is_empty()
                || !value.contains('@')
                || value
                    .chars()
                    .any(|c| c.is_whitespace() || matches!(c, '<' | '>'))
            {
                return Err(Error::bad("Invalid message reference."));
            }
            Ok(value.to_owned())
        })
        .collect()
}

pub fn compose(files: &Files, connection: &Connection, args: &Value) -> Result<Value> {
    let mut builder = MessageBuilder::new().from(address(&args["from"])?);
    let mut recipients = 0;
    for field in ["to", "cc", "bcc", "replyTo"] {
        if args.get(field).is_none() {
            continue;
        }
        let list = addresses(&args[field])?;
        if field != "replyTo" {
            recipients += list.len();
        }
        let list = Address::new_list(list);
        builder = match field {
            "to" => builder.to(list),
            "cc" => builder.cc(list),
            "bcc" => builder.bcc(list),
            _ => builder.reply_to(list),
        };
    }
    if recipients == 0 || recipients > 500 {
        return Err(Error::bad("Choose between one and 500 recipients."));
    }
    builder = builder.subject(header(text(args, "subject"), 4000)?.to_owned());
    if let Some(value) = args["text"].as_str() {
        builder = builder.text_body(value.to_owned());
    }
    if let Some(value) = args["html"].as_str() {
        builder = builder.html_body(value.to_owned());
    }
    if args["text"].as_str().is_none() && args["html"].as_str().is_none() {
        return Err(Error::bad("Provide text or HTML content."));
    }
    for field in ["inReplyTo", "references"] {
        if args.get(field).is_none() {
            continue;
        }
        let ids = message_ids(&args[field])?;
        builder = if field == "inReplyTo" {
            builder.in_reply_to(ids)
        } else {
            builder.references(ids)
        };
    }
    let message_id = match args["messageId"].as_str() {
        Some(value) => message_ids(&json!([value]))?.remove(0),
        None => {
            let key = text(args, "messageKey");
            if key.is_empty() {
                format!("{}@cairn.invalid", id())
            } else {
                format!(
                    "{}@cairn.invalid",
                    crate::auth::hex_digest(&format!("{}:{key}", connection.id))
                )
            }
        }
    };
    builder = builder.message_id(message_id.clone());
    let attachments = args["attachments"].as_array().cloned().unwrap_or_default();
    if attachments.len() > 100 {
        return Err(Error::bad("Attach at most 100 files."));
    }
    let mut total = text(args, "text").len() + text(args, "html").len();
    let mut intent = args.clone();
    if let Some(object) = intent.as_object_mut() {
        object.remove("instance");
        object.remove("messageKey");
    }
    let mut attachment_intents = Vec::new();
    for attachment in attachments {
        let bytes = files.bytes(connection, text(&attachment, "id"))?;
        let mut part = attachment.clone();
        part.as_object_mut()
            .ok_or_else(|| Error::bad("Attachments must be objects."))?
            .remove("id");
        part["sha256"] = json!(hex::encode(Sha256::digest(&bytes)));
        attachment_intents.push(part);
        total += bytes.len();
        if total > 28 * 1024 * 1024 {
            return Err(Error::too_large(
                "Message contents exceed 28 MB before MIME encoding.",
            ));
        }
        let media_type = header(text(&attachment, "mediaType"), 150)?.to_owned();
        if media_type.is_empty() || !media_type.contains('/') {
            return Err(Error::bad("Provide an attachment mediaType."));
        }
        if let Some(cid) = attachment["contentId"].as_str() {
            header(cid, 200)?;
            if cid.is_empty() || cid.contains(['<', '>']) {
                return Err(Error::bad("Invalid inline content ID."));
            }
            builder = builder.inline(media_type, cid.to_owned(), bytes);
        } else {
            let name = header(text(&attachment, "name"), 200)?;
            if name.is_empty() {
                return Err(Error::bad("Provide an attachment name."));
            }
            builder = builder.attachment(media_type, name.to_owned(), bytes);
        }
    }
    let bytes = builder.write_to_vec()?;
    if bytes.len() > 35 * 1024 * 1024 {
        return Err(Error::too_large("The encoded mail exceeds 35 MB."));
    }
    let mut file = files.create(connection, "message.eml", "message/rfc822", &bytes)?;
    file["messageId"] = json!(format!("<{message_id}>"));
    intent["attachments"] = json!(attachment_intents);
    file["contentDigest"] = json!(crate::auth::hex_digest(&serde_json::to_string(&intent)?));
    Ok(file)
}
