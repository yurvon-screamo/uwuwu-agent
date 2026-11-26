//! Сброс буфера ввода бота (#1596): один ход сессии на накопленный
//! батч, материал первым, отказы — до реплая, по одному ТГ-исходу на
//! отказ. Чистая сборка строк живёт в `usecase::bots::buffer`.

use super::runner::{
    forward_line, run_turn, send, BotDeps, REFUSE_FAILED, REFUSE_KIND, REFUSE_OVERSIZE,
    TG_MAX_FILE_BYTES,
};
use uwuwu_usecase::bots::{classify_message, Incoming, InputBuffer, MediaKind, TgMedia, TgMessage};
use uwuwu_usecase::UsecaseResult;

/// Готовый фрагмент батча: промпт или явный отказ владельцу.
pub(crate) enum Fragment {
    Prompt(String),
    Refusal(String),
}

fn text_fragment(text: &str, forward: &Option<String>, is_edit: bool) -> Fragment {
    let marker = forward_line(forward);
    let body = if is_edit {
        format!("Правка сообщения: {text}")
    } else {
        text.to_string()
    };
    Fragment::Prompt(format!("{marker}{body}"))
}

/// Медиа-фрагмент: скачивание и asset store на сбросе (#1596). Отказ —
/// перманентный, батч продолжается без этого элемента.
pub(crate) async fn media_fragment(deps: &BotDeps, message: &TgMessage) -> Fragment {
    let Some(media) = &message.media else {
        return text_fragment(
            message.text.as_deref().unwrap_or_default(),
            &message.forward,
            false,
        );
    };
    let url = match prepare_url(deps, media).await {
        Ok(url) => url,
        Err(refusal) => return refusal,
    };
    tracing::info!(event = "bot.media_delivered", slug = %deps.slug,
        kind = ?media.kind, "Файл забран и приложен к контексту сессии");
    let caption = message
        .caption
        .as_deref()
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .unwrap_or("Подписи нет");
    Fragment::Prompt(format!(
        "{}Владелец прислал файл: {url}\n{caption}\n\
         Если оформляешь заявку — вложи эту ссылку в тело задачи (markdown), \
         чтобы контент попал в разбор.",
        forward_line(&message.forward)
    ))
}

/// Скачивание → asset store с явными отказами по лекалу #1171.
async fn prepare_url(deps: &BotDeps, media: &TgMedia) -> Result<String, Fragment> {
    if media.kind == MediaKind::Other {
        tracing::info!(event = "bot.media_refused", slug = %deps.slug,
            kind = "other", "Медиа-тип не поддержан — явный отказ");
        return Err(Fragment::Refusal(REFUSE_KIND.to_string()));
    }
    if media.file_size.is_some_and(|size| size > TG_MAX_FILE_BYTES) {
        tracing::info!(event = "bot.media_oversize", slug = %deps.slug,
            kind = "pre_check", "Файл больше потолка Bot API — явный отказ");
        return Err(Fragment::Refusal(REFUSE_OVERSIZE.to_string()));
    }
    let bytes = match deps.tg.download_file(&media.file_id).await {
        Ok(bytes) => bytes,
        Err(uwuwu_usecase::UsecaseError::TelegramTooBig(_)) => {
            tracing::info!(event = "bot.media_oversize", slug = %deps.slug,
                kind = "get_file_400", "getFile: file is too big — явный отказ");
            return Err(Fragment::Refusal(REFUSE_OVERSIZE.to_string()));
        }
        Err(_) => {
            tracing::warn!(event = "bot.media_refused", slug = %deps.slug,
                kind = "download_failed", "Скачивание не удалось — явный отказ");
            return Err(Fragment::Refusal(REFUSE_FAILED.to_string()));
        }
    };
    deps.assets
        .put(&deps.slug, media.kind, media.file_name.as_deref(), &bytes)
        .await
        .map_err(|_| {
            tracing::warn!(event = "bot.media_refused", slug = %deps.slug,
                kind = "upload_failed", "Загрузка ассета не удалась — явный отказ");
            Fragment::Refusal(REFUSE_FAILED.to_string())
        })
}

/// Сброс накопленного ввода: отказы уходят первыми, затем один промпт
/// вечной сессии и один ответ владельцу. Offset двигается вызывающим
/// только после успеха.
pub(crate) async fn flush(deps: &BotDeps, buffer: &InputBuffer) -> UsecaseResult<()> {
    let items = buffer.ordered();
    let owner = deps.live.view().owner;
    let mut prompts = Vec::new();
    let mut refusals = Vec::new();
    let mut chat_id = None;
    for item in &items {
        let Some(incoming) = classify_message(&item.message, owner.as_deref()) else {
            continue;
        };
        chat_id = Some(item.message.chat_id);
        let fragment = match incoming {
            Incoming::Media { .. } => media_fragment(deps, &item.message).await,
            Incoming::Text(text) => text_fragment(&text, &item.message.forward, item.is_edit),
        };
        match fragment {
            Fragment::Prompt(text) => prompts.push(text),
            Fragment::Refusal(text) => refusals.push(text),
        }
    }
    let Some(chat_id) = chat_id else {
        return Ok(());
    };
    for refusal in &refusals {
        send(deps, chat_id, refusal).await?;
    }
    let prompt = prompts.join("\n\n");
    if !prompt.is_empty() {
        run_turn(deps, chat_id, &prompt).await?;
    }
    tracing::info!(event = "bot.buffer_flushed", slug = %deps.slug,
        items = items.len(), refusals = refusals.len(),
        "Батч ввода доставлен в сессию бота");
    Ok(())
}
