use actix_multipart::{Field, Multipart};
use diesel::{insert_into, prelude::*};
use std::{fs::File, io::Write, path::Path};
use uuid::Uuid;

use crate::schema::sounds::dsl::sounds as sounds_dsl;
use actix_web::{
    get, post,
    web::{self, Data, Json},
    Error, HttpResponse,
};
use serde::{Deserialize, Serialize};
use serenity::{futures::TryStreamExt, model::id::GuildId};
use songbird::{
    driver::Bitrate,
    input::{self, cached::Compressed},
};

use crate::{app_state::AppState, models::Sound, schema::sounds, storage::get_audio_path};

#[derive(Deserialize, Serialize, Clone, Copy)]
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
pub struct ErrorPayload {
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlaySoundResponse {
    sound_id: String,
    client: Client,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UploadSuccess {
    id: String,
    filename: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UploadFailure {
    filename: String,
    reason: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UploadResponse {
    successful: Vec<UploadSuccess>,
    failed: Vec<UploadFailure>,
    tags: Vec<String>,
}

#[get("/sounds")]
pub async fn sounds_handler(data: Data<AppState>) -> Result<HttpResponse, Error> {
    let database_connection = &data.database_connection;
    // TODO: fetch tags as well, add left_join
    let query = sounds::table;
    let sounds = match query.load::<Sound>(database_connection) {
        Ok(sounds) => sounds,
        Err(reason) => {
            eprintln!("Failed to fetch sounds from database. Reason: {:?}", reason);
            return Ok(HttpResponse::InternalServerError().json(ErrorPayload {
                message: "Server failed to fetch sounds from database.".to_string(),
            }));
        }
    };

    Ok(HttpResponse::Ok().json(sounds))
}

#[post("/play-sound")]
pub async fn play_sound_handler(
    data: Data<AppState>,
    json: Json<PlaySoundPayload>,
) -> Result<HttpResponse, Error> {
    let ctx_mutex = data.discord_ctx.try_lock().unwrap();
    let ctx = ctx_mutex.as_ref().unwrap();

    let manager = songbird::get(ctx)
        .await
        .expect("Songbird Voice client placed in at initialisation.")
        .clone();

    let guild_id: GuildId = GuildId {
        // server da maconha guild id
        0: 194951764045201409,
    };

    if let Some(handler_lock) = manager.get(guild_id) {
        let mut handler = handler_lock.lock().await;

        // TODO: fetch audio folder path from env var
        let audio_folder_path = Path::new("data/audio");
        let audio_path = get_audio_path(audio_folder_path, json.sound_id.clone());

        if !audio_path.exists() {
            return Ok(HttpResponse::InternalServerError().json(ErrorPayload {
                message: format!("Audio is missing for sound with id: {}", json.sound_id),
            }));
        }

        let sound_src = Compressed::new(
            input::ffmpeg(audio_path).await.expect("Link may be dead."),
            Bitrate::BitsPerSecond(128_000),
        )
        .expect("ffmpeg parameters to be properly defined");

        let track = handler.play_source(sound_src.into());
        let _ = track.set_volume(1.0);
    } else {
        return Ok(HttpResponse::BadRequest().json(ErrorPayload {
            message: "Bot has to join a voice channel first.".to_string(),
        }));
    }

    Ok(HttpResponse::Ok().json(PlaySoundResponse {
        sound_id: json.sound_id.to_owned(),
        client: json.client.to_owned(),
    }))
}

async fn upload_payload_file(
    mut field: Field,
    payload_filename: &str,
    audio_folder_path: &Path,
    database_connection: &SqliteConnection,
) -> Result<UploadSuccess, Error> {
    let original_filename = Path::new(payload_filename);
    let extension = original_filename
        .extension()
        .expect("File extension to be extractable")
        .to_str()
        .unwrap()
        .to_string();

    let file_name = Uuid::new_v4().to_string();
    let mut filepath = audio_folder_path.join(&file_name);
    filepath.set_extension(&extension);

    let mut file = web::block(|| File::create(filepath))
        .await
        .expect("Failed to create file")?;

    while let Some(chunk) = field.try_next().await? {
        // filesystem operations are blocking, we have to use threadpool
        file = web::block(move || file.write_all(&chunk).map(|_| file))
            .await
            .expect("Failed to write to file")?;
    }

    let id = Uuid::new_v4().to_string();
    let sound_name = original_filename
        .file_stem()
        .expect("Failed to get file stem")
        .to_str()
        .unwrap()
        .to_string();

    let sound_record = Sound {
        id,
        file_name,
        extension: format!(".{}", extension),
        // TODO: introduce a proper hashing mechanism
        file_hash: "hash".to_string(),
        name: sound_name,
    };

    insert_into(sounds_dsl)
        .values(&sound_record)
        .execute(database_connection)
        .expect("Failed to insert sound in database.");

    Ok(UploadSuccess {
        id: sound_record.id.clone(),
        filename: payload_filename.to_string(),
    })
}

#[post("/upload")]
async fn upload_handler(
    mut payload: Multipart,
    data: Data<AppState>,
) -> Result<HttpResponse, Error> {
    let _valid_extensions = vec!["mp3", "wav", "ogg", "webm"];
    let audio_folder_path = Path::new("data/audio");

    let mut successful_uploads: Vec<UploadSuccess> = vec![];
    let mut failed_uploads: Vec<UploadFailure> = vec![];

    while let Some(field) = payload.try_next().await? {
        // A multipart/form-data stream has to contain `content_disposition`
        // this is where we'll be able to fetch the file name and
        // the key name for other parts of the payload
        let content_disposition = field
            .content_disposition()
            .expect("Content disposition to be present");

        // TODO: Check if content type is among the allowed ones
        // make sure to check the magic number of the payload buf
        // and that they intersect with the valid extensions list
        //
        // checkout the package `infer`, which is already installed
        // on this project
        let _content_type = field.content_type();
        let field_key = content_disposition
            .get_name()
            .expect("Content disposition to have a name");

        if field_key == "tags" {
            // TODO: insert tags to database
            break;
        }

        let payload_filename = content_disposition
            .get_filename()
            .expect("Uploaded file to always contain a filename.");

        let upload_result = upload_payload_file(
            field,
            payload_filename,
            audio_folder_path,
            &data.database_connection,
        )
        .await;

        match upload_result {
            Ok(successful) => successful_uploads.push(successful),
            Err(failure) => failed_uploads.push(UploadFailure {
                filename: payload_filename.to_string(),
                reason: failure.to_string(),
            }),
        }
    }

    Ok(HttpResponse::Ok().json(UploadResponse {
        successful: successful_uploads,
        failed: failed_uploads,
        tags: vec![],
    }))
}
