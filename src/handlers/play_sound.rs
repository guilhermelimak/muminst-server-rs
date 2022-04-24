use std::path::Path;

use actix_web::{
    post,
    web::{self, Data, Json},
    Error, HttpResponse,
};
use log::debug;
use serde::{Deserialize, Serialize};
use teloxide::{prelude::*, types::InputFile};

use crate::{actions::sounds::fetch_sound_by_id, app_state::AppState, discord::actor::PlayAudio};

#[derive(Deserialize, Serialize, Clone, Copy, Debug)]
#[serde(rename_all = "camelCase")]
enum Client {
    Discord,
    Telegram,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaySoundPayload {
    sound_id: String,
    client: Client,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ErrorPayload {
    message: String,
}

#[derive(Serialize, Debug)]
#[serde(rename_all = "camelCase")]
struct PlaySoundResponse {
    sound_id: String,
    client: Client,
}

#[post("/play-sound")]
pub async fn play_sound_handler(
    data: Data<AppState>,
    json: Json<PlaySoundPayload>,
) -> Result<HttpResponse, Error> {
    let audio_folder_path = Path::new(&data.audio_folder_path);
    let data_clone = data.clone();
    let sound_id = json.sound_id.clone();
    let sound = web::block(move || {
        let database_connection = &data_clone
            .database_pool
            .get()
            .expect("couldn't get db connection from pool");

        fetch_sound_by_id(sound_id, database_connection)
    })
    .await?;

    let sound = match sound {
        Some(sound) => sound,
        None => {
            return Ok(HttpResponse::ExpectationFailed().json(ErrorPayload {
                message: format!("Failed to find sound with id: {}", json.sound_id),
            }));
        }
    };

    let audio_path = {
        let sound = sound.clone();
        let mut path = audio_folder_path.join(sound.file_name);
        path.set_extension(sound.extension);
        path
    };

    if !audio_path.exists() {
        return Ok(HttpResponse::InternalServerError().json(ErrorPayload {
            message: format!("Audio is missing for sound with id: {}", json.sound_id),
        }));
    }

    debug!("json client is {:?}", &json.client);

    match json.client {
        Client::Discord => {
            data.discord_actor_addr
                .send(PlayAudio { audio_path, sound })
                .await
                .expect("Failed to play audio");
        }
        Client::Telegram => {
            let chat_id = data.telegram_chat_id.clone();
            let file = InputFile::file(&audio_path).file_name(sound.name.clone());
            debug!(
                "sending audio at {:?} to telegram chat id: {:?}",
                sound.name, chat_id
            );

            let _ = data.telegram_bot.send_audio(chat_id, file).await;
        }
    }

    Ok(HttpResponse::Ok().json(PlaySoundResponse {
        sound_id: json.sound_id.clone(),
        client: json.client,
    }))
}
