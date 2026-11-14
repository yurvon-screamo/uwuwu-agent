//! GitHubStore read-side trait-impl: list/resolve/body/discover + делегация
//! write-side (github_task_write.rs). Структура/конвертация — github_task.rs.

use super::github_task::GitHubStore;
use super::task_store::{CreateTask, EditTask, StoreResult, TaskStore, ThreadEntry};
use super::{StoreError, Task, TaskBoard};
use crate::store::github_gh::gh_json;
use serde_json::Value;

#[async_trait::async_trait]
impl TaskStore for GitHubStore {
    async fn list(&self, board: &TaskBoard) -> StoreResult<Vec<Task>> {
        // Явный query-string (URL-encoded colon): gh -f/-F форма на GET
        // давала искажённую выборку (живой e2e 13.11 — свежий issue не
        // попадал в -f labels, ручной query-string видел).
        let label = crate::store::github_task::urlencoded(&self.label_of(board));
        // Полная выборка с пагинацией (контракт трейта): uwuwu-доска
        // превышает первую страницу 100 (332 items).
        let mut all_rows: Vec<Value> = Vec::new();
        let mut page = 1usize;
        loop {
            let data = gh_json(
                &[
                    "api",
                    &format!(
                        "repos/{}/issues?labels={label}&state=open&per_page=100&page={page}",
                        self.repo
                    ),
                ],
                None,
            )
            .await?;
            let Some(arr) = data.as_array() else {
                break;
            };
            let fetched = arr.len();
            all_rows.extend(arr.iter().cloned());
            if fetched < 100 {
                break;
            }
            page += 1;
        }
        let data = Value::Array(all_rows);
        let mut out = Vec::new();
        if let Value::Array(items) = data {
            for issue in &items {
                if issue.get("pull_request").is_some() {
                    continue;
                }
                out.push(self.convert(issue));
            }
        }
        Ok(out)
    }

    async fn resolve(&self, board: &TaskBoard, needle: &str) -> Result<Task, StoreError> {
        let tasks = self.list(board).await?;
        let exact = needle.trim_start_matches(['-', '#']);
        if let Some(t) = tasks.iter().find(|t| t.id == exact) {
            return Ok(t.clone());
        }
        // Точная форма — issue-номер (закрытые строки включительно):
        // номер выше substring-фоллбека, иначе закрытая строка «не найдено»,
        // а её номер перебивается substring чужого тайтла (1048/1088).
        if let Some(number) = numeric_needle(exact) {
            // Нет issue с таким номером — substring по доске как раньше
            if let Some(t) = self.issue_by_number(board, &number.to_string()).await? {
                return Ok(t);
            }
        }
        let matched: Vec<&Task> = tasks
            .iter()
            .filter(|t| t.title.to_lowercase().contains(&needle.to_lowercase()))
            .collect();
        match matched.as_slice() {
            [one] => Ok((*one).clone()),
            [] => Err(StoreError::ListCheckedButNotFound {
                needle: needle.to_string(),
            }),
            many => Err(StoreError::Other(anyhow::anyhow!(
                "задача «{needle}»: неоднозначно на {} строках ({}) — уточни номер issue",
                many.len(),
                many.iter()
                    .map(|t| format!("#{} «{}»", t.id, t.title))
                    .collect::<Vec<_>>()
                    .join(", ")
            ))),
        }
    }

    async fn body_get(&self, _board: &TaskBoard, task: &Task) -> StoreResult<String> {
        self.issue_body(&task.id).await
    }

    /// Полная перезапись тела с сохранением fm-карточки (шапка
    /// `---\n…\n---`): если new_md не несёт собственный fm — переносится
    /// текущий; карточки не было — пустой блок не создаётся; new_md со
    /// своим fm пишется как есть (append-путь: current+md уже с fm).
    async fn body_replace(
        &self,
        _board: &TaskBoard,
        task: &Task,
        new_md: &str,
        base: &str,
    ) -> StoreResult<Task> {
        let current = self.issue_body(&task.id).await?;
        if !base_matches_snapshot(&current, base) {
            anyhow::bail!(
                "тело изменилось с момента снапшота (--base) — перечитай task get и повтори"
            );
        }
        let composed = compose_body_with_fm(new_md, &current);
        self.set_issue_body(&task.id, &composed).await?;
        Ok(self.issue_fresh(&task.id).await?)
    }

    /// Тред = issue comments (плоский список; reply-семантика — цитатный
    /// маркер, вложенности нет).
    async fn thread(&self, _board: &TaskBoard, task: &Task) -> StoreResult<Vec<ThreadEntry>> {
        let number = &task.id;
        let mut out = Vec::new();
        let mut page = 1usize;
        loop {
            let data = gh_json(
                &[
                    "api",
                    "-X",
                    "GET",
                    &format!("repos/{}/issues/{number}/comments", self.repo),
                    "-F",
                    "per_page=100",
                    "-F",
                    &format!("page={page}"),
                ],
                None,
            )
            .await?;
            let Some(items) = data.as_array() else {
                break;
            };
            let fetched = items.len();
            for c in items {
                out.push(ThreadEntry {
                    id: c["id"].to_string(),
                    reply_to: None,
                    author: c
                        .pointer("/user/login")
                        .and_then(|l| l.as_str())
                        .unwrap_or("unknown")
                        .to_string(),
                    text: c["body"].as_str().unwrap_or("").to_string(),
                });
            }
            if fetched < 100 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    async fn body_attrs(
        &self,
        _board: &TaskBoard,
        _task: &Task,
    ) -> StoreResult<std::collections::HashMap<String, String>> {
        Ok(Default::default())
    }

    async fn discover_boards(&self) -> StoreResult<Vec<TaskBoard>> {
        let labels = gh_json(&["api", &format!("repos/{}/labels", self.repo)], None).await?;
        let mut out = Vec::new();
        if let Value::Array(items) = labels {
            for l in &items {
                let name = l["name"].as_str().unwrap_or("");
                if let Some(project) = name.strip_prefix("proj:") {
                    out.push(TaskBoard {
                        project: project.to_string(),
                    });
                }
            }
        }
        Ok(out)
    }

    async fn create(&self, board: &TaskBoard, input: &CreateTask) -> Result<Task, StoreError> {
        self.create_impl(board, input).await
    }

    async fn edit(&self, board: &TaskBoard, task: &Task, patch: &EditTask) -> StoreResult<Task> {
        self.edit_impl(board, task, patch).await
    }

    async fn comment(
        &self,
        board: &TaskBoard,
        task: &Task,
        text: &str,
        reply_to: Option<&str>,
    ) -> Result<super::task_store::CommentInsert, StoreError> {
        self.comment_impl(board, task, text, reply_to).await
    }

    async fn claim(&self, board: &TaskBoard, task: &Task, session: &str) -> StoreResult<Task> {
        self.claim_impl(board, task, session).await
    }

    async fn close(&self, board: &TaskBoard, task: &Task, note: Option<&str>) -> StoreResult<Task> {
        self.close_impl(board, task, note).await
    }

    async fn set_body_attr(
        &self,
        board: &TaskBoard,
        task: &Task,
        key: &str,
        value: &str,
    ) -> StoreResult<()> {
        self.set_body_attr_impl(board, task, key, value).await
    }
}

/// Числоподобный needle → issue-номер (точная форма резолва: «1050»,
/// «#1050»; «abc» и префиксы row-id — None → substring-фоллбек).
fn numeric_needle(exact: &str) -> Option<u64> {
    exact.parse::<u64>().ok()
}

/// Снапшот свежести → сырое тело: срезка обёртки `task get` (шапка
/// «Задача:/…/Строка:   issue #N» + пустая строка; help: «cli срезает
/// обёртку при сверке сам») и CRLF → LF. Снапшоты сырого тела (gh api)
/// и чистого тела проходят без срезки. Детектор шапки требует шапочную
/// форму строки-маркера («Строка:» + 3 пробела + «issue #» + цифры):
/// телесные «Строка:…» произвольной формы (таблицы и пр.) raw-снапшот
/// не срезают; теоретический предел — raw-тело со строкой точь-в-точь
/// шапочной формы после «Задача:»-старта — ложная срезка, направление
/// безопасное (отказ свежести, не запись).
fn snapshot_to_body(snapshot: &str) -> String {
    let normalized = snapshot.replace("\r\n", "\n");
    if normalized.starts_with("Задача:") {
        // Шапка get-принта идёт первой; телесные вхождения позже шапки
        // не помешали бы, но сырой снапшот без get-шапки не срезается
        let mut offset = 0usize;
        for line in normalized.split('\n') {
            if is_wrapper_marker_line(line) {
                let after_marker = &normalized[offset + line.len()..];
                return after_marker.trim_start_matches('\n').to_string();
            }
            offset += line.len() + 1;
        }
    }
    normalized
}

/// Шапочная форма маркера get-принта (cmd/hub_task.rs):
/// «Строка:   issue #<номер>» — ровно 3 пробела и цифры до конца строки.
fn is_wrapper_marker_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("Строка:") else {
        return false;
    };
    let Some(digits) = rest.strip_prefix("   issue #") else {
        return false;
    };
    !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
}

/// Сверка снапшота свежести с текущим телом. Хвостовые переводы строк
/// не несут контента: get-принт (println) добавляет финальный \n, сырой
/// issue.body его не всегда имеет (офф-бай-один отказов --base).
/// U+200B и прочий контент сравниваются дословно — одинаковы в обеих
/// сторонах, санитизация не нужна (1096/1100).
fn base_matches_snapshot(current: &str, snapshot: &str) -> bool {
    snapshot_to_body(snapshot).trim_end() == current.replace("\r\n", "\n").trim_end()
}

/// fm-карточка тела: `---\n…\n---\n` в начале строки (Option — карточки
/// может не быть: issue создан руками в UI без шапки).
fn fm_block(body: &str) -> Option<&str> {
    let rest = body.strip_prefix("---\n")?;
    let mut end = 4usize;
    for line in rest.split_inclusive('\n') {
        end += line.len();
        if line.trim_end() == "---" {
            return Some(&body[..end]);
        }
    }
    None
}

/// Сборка нового тела с сохранением fm-карточки. Правила:
/// - new_md с собственным fm — пишется как есть (append-путь передаёт
///   current+md: карточка уже внутри, повторный prepend задвоил бы блок —
///   High ревью хвоста 5);
/// - new_md без fm, карточка есть — fm переносится сверху (replace-путь:
///   контентное тело не затирает карточку статусов);
/// - карточки нет ни там, ни там — пустой `---\n---` не создаётся
///   (Common ревью: мусорный блок жил в rest_md навсегда).
fn compose_body_with_fm(new_md: &str, current: &str) -> String {
    if new_md.starts_with("---\n") {
        return new_md.to_string();
    }
    match fm_block(current) {
        Some(fm) => format!("{fm}\n{new_md}"),
        None => new_md.to_string(),
    }
}

#[cfg(test)]
mod base_snapshot_tests {
    use super::base_matches_snapshot;

    /// Снапшот в форме get-принта (cmd/hub_task.rs): шапка + пустая строка
    /// + тело (fm-блок — часть тела issue).
    fn get_snapshot(title: &str, status: &str, number: &str, body: &str) -> String {
        let mut s = format!("Задача:   {title}\nСтатус:   {status}\n");
        s.push_str(&format!("Тип:      bug\nСтрока:   issue #{number}\n\n"));
        s.push_str(body);
        s.push('\n'); // println добавляет финальный \n
        s
    }

    const BODY_WITH_FM: &str = "---\npriority: high\nautodone: true\n---\n## Постановка\n\nтекст\n";

    #[test]
    fn полный_get_снапшот_матчится_обёртка_срезается() {
        let current = BODY_WITH_FM;
        assert!(
            base_matches_snapshot(current, &get_snapshot("Т", "In Progress", "1097", current)),
            "снапшот task get (обёртка + хвостовой \\n) — матчится"
        );
    }

    #[test]
    fn тело_без_fm_и_чистый_снапшот_матчатся() {
        let current = "плоское тело\n";
        assert!(base_matches_snapshot(current, "плоское тело\n"));
        assert!(base_matches_snapshot(current, "плоское тело"));
    }

    #[test]
    fn хвостовой_newline_не_ломает_сверку() {
        let current = "тело без хвоста";
        let snapshot = format!("Задача:   Т\nСтатус:   Todo\nСтрока:   issue #9\n\n{current}\n");
        assert!(base_matches_snapshot(current, &snapshot));
    }

    #[test]
    fn raw_jq_артефакт_двойной_хвост_не_ломает_сверку_1109() {
        // Инстанс #1109: base = `gh api --jq .body` — jq печатает тело
        // (уже с хвостовым \n) плюс свой \n → двойной хвост 0a0a;
        // вербатим-сверка отказывала, trim_end-сверка проходит
        let current = BODY_WITH_FM;
        let raw = format!("{current}\n\n");
        assert!(base_matches_snapshot(current, &raw));
    }

    #[test]
    fn crlf_снапшот_нормализуется() {
        let current = "---\nstatus: Todo\n---\nконтент\n";
        let snapshot = "Задача:   Т\r\nСтатус:   Todo\r\nСтрока:   issue #9\r\n\r\n---\r\nstatus: Todo\r\n---\r\nконтент\r\n";
        assert!(base_matches_snapshot(current, snapshot));
    }

    #[test]
    fn zero_width_пробелы_сравниваются_дословно_и_не_мешают() {
        // U+200B в обеих сторонах (инстанс #56): сверка дословная — расхождений
        // изнутри сверки нет
        let current = "текст \u{200b}код\u{200b} хвост\n";
        let snapshot = format!("Задача:   Т\nСтатус:   Todo\nСтрока:   issue #56\n\n{current}\n");
        assert!(base_matches_snapshot(current, &snapshot));
    }

    #[test]
    fn телесный_задача_без_шапки_не_режется() {
        let current = "Задача: сделать Х\n\nшаги\n";
        assert!(base_matches_snapshot(current, current));
        assert!(base_matches_snapshot(
            current,
            "Задача: сделать Х\n\nшаги\n\n\n"
        ));
    }

    #[test]
    fn телесная_строка_после_шапки_не_путает_срезку() {
        // «Строка:» в контенте произвольной формы — срезка по шапочной форме
        let current = "таблица:\nСтрока:  (issue)\nконец\n";
        let snapshot = format!("Задача:   Т\nСтатус:   Todo\nСтрока:   issue #9\n\n{current}");
        assert!(base_matches_snapshot(current, &snapshot));
    }

    #[test]
    fn raw_снапшот_с_обоими_маркерами_не_режется() {
        // Low-ревью 1097: сырой снапшот (gh api) тела, которое само
        // начинается «Задача:» и содержит телесное «\nСтрока:» произвольной
        // формы — не срезается (детектор требует шапочную форму
        // «Строка:   issue #<цифры>»), сверка идёт как есть
        let current = "Задача: сборка\n\nтаблица:\nСтрока:  (issue)\nконец\n";
        assert!(base_matches_snapshot(current, current));
        // get-снапшот того же тела — срезка по шапочному маркеру корректна
        let snapshot = format!("Задача:   Т\nСтатус:   Todo\nСтрока:   issue #9\n\n{current}");
        assert!(base_matches_snapshot(current, &snapshot));
    }

    #[test]
    fn шапочная_форма_требует_цифры_и_ровно_три_пробела() {
        let current = "Задача: сделать Х\n\nтаблица:\nСтрока:  (issue)\nконец\n";
        assert!(base_matches_snapshot(current, current));
        // теоретический предел детектора (документирован в док-комменте):
        // raw-тело со строкой точь-в-точь шапочной формы после «Задача:»-
        // старта срезается → ложный отказ свежести; направление безопасное
        // (отказ, не запись), реальный get-снапшот того же тела корректен —
        // шапка get-принта всегда раньше телесной строки
        let tricky = "Задача: Х\n\nСтрока:   issue #5\nконец\n";
        assert!(!base_matches_snapshot(tricky, tricky));
        let snapshot = format!("Задача:   Т\nСтатус:   Todo\nСтрока:   issue #9\n\n{tricky}");
        assert!(base_matches_snapshot(tricky, &snapshot));
    }

    #[test]
    fn изменившееся_тело_отказ() {
        let snapshot = get_snapshot("Т", "Todo", "1097", "---\nstatus: Todo\n---\nстарое\n");
        assert!(!base_matches_snapshot(
            "---\nstatus: Todo\n---\nновое\n",
            &snapshot
        ));
    }

    #[test]
    fn параллельная_правка_хвоста_ловится() {
        let snapshot = get_snapshot("Т", "Todo", "1097", "контент А\n");
        assert!(!base_matches_snapshot("контент B\n", &snapshot));
    }
}

#[cfg(test)]
mod resolve_needle_tests {
    use super::numeric_needle;

    #[test]
    fn числоподобные_формы_парсятся() {
        assert_eq!(numeric_needle("1050"), Some(1050));
        assert_eq!(numeric_needle("1097"), Some(1097));
    }

    #[test]
    fn не_число_и_префиксы_это_substring_фоллбек() {
        assert_eq!(numeric_needle("abc"), None);
        assert_eq!(numeric_needle("20261114141050xyz"), None);
        // «-» и «#» срезаются до numeric-проверки (trim в resolve)
        assert_eq!(numeric_needle("42-foo"), None);
    }
}

#[cfg(test)]
mod fm_compose_tests {
    use super::{compose_body_with_fm, fm_block};

    const CARD: &str = "---\nstatus: In Progress\nbranch: work/x\n---\n\nтело задачи\n";

    #[test]
    fn append_не_задваивает_fm() {
        // append-путь: new_md = current + добавка (уже с карточкой)
        let new_md = format!("{CARD}добавка\n");
        let out = compose_body_with_fm(&new_md, CARD);
        assert_eq!(out, new_md, "тело с fm пишется как есть");
        assert_eq!(out.matches("---\n").count(), 2, "ровно один fm-блок");
    }

    #[test]
    fn replace_переносит_карточку_на_контент_без_fm() {
        let out = compose_body_with_fm("новый контент\n", CARD);
        assert!(out.starts_with("---\nstatus: In Progress"));
        assert!(out.contains("новый контент"));
        assert_eq!(out.matches("---\n").count(), 2);
    }

    #[test]
    fn без_карточки_нет_пустого_блока() {
        let out = compose_body_with_fm("контент", "плоский старый текст\n");
        assert_eq!(out, "контент", "пустой ---\\n--- не создаётся");
    }

    #[test]
    fn новый_fm_побеждает_старый() {
        let out = compose_body_with_fm("---\nstatus: Todo\n---\n\nx\n", CARD);
        assert!(out.starts_with("---\nstatus: Todo"));
    }

    #[test]
    fn fm_block_границы() {
        assert!(fm_block("нет карточки").is_none());
        assert!(fm_block("---\n---\n").is_some());
        let b = fm_block(CARD).unwrap();
        assert_eq!(b, "---\nstatus: In Progress\nbranch: work/x\n---\n");
    }
}
