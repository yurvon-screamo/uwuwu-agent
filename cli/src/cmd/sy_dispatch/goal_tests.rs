//! Тесты goal: карточка цели.

use super::goal::format_goal_card;

use crate::siyuan_task::SyBoard;

use super::task_tests::header_task;

/// Карточка цели (контракт goal add/edit): статус/строка/тайтл/доска.
#[test]
fn goal_card_shape() {
    let board = SyBoard {
        doc_id: "goal-board".into(),
        av_id: "av-goal".into(),
        block_id: "goal-block".into(),
        hpath: "/uwuwu/Цели uwuwu".into(),
        notebook: "nb".into(),
    };
    let goal = crate::siyuan_task::SyTask {
        item_id: "20261111111111-goal1".into(),
        title: "цель-раз".into(),
        status: "in progress".into(),
        ..header_task()
    };
    let card = format_goal_card(&board, &goal);
    for line in [
        "Цель:     цель-раз",
        "Статус:   in progress",
        "Строка:   20261111111111-goal1",
        "Доска:    /uwuwu/Цели uwuwu",
    ] {
        assert!(card.contains(line), "нет строки «{line}» в:\n{card}");
    }
}
