//! person: персона владельца (SiYuan-док).

use crate::cmd::sy_dispatch::guards::{collect_text, read_body_input};
use crate::cmd::sy_dispatch::task::helpers::print_final_body;
use crate::cmd::cli::PersonSub;
use crate::siyuan_person as person;
use crate::siyuan_person::person_target;
use crate::siyuan::SiYuan;
use anyhow::{bail, Result};

pub(crate) async fn person_run(command: PersonSub) -> Result<()> {
    let sy = SiYuan::from_env()?;
    let target = person_target();
    match command {
        PersonSub::Show => {
            let md = person::show(&sy, &target).await?;
            print!("{md}");
            Ok(())
        }
        PersonSub::Append { stdin, text } => {
            let md = collect_text(stdin, &text)?;
            if md.trim().is_empty() {
                bail!("append: нужен текст (позиционный или --stdin)");
            }
            let doc_id = person::append(&sy, &target, &md).await?;
            println!("дописано: {}:{} ({doc_id})", target.notebook, target.hpath);
            // контракт ответов: финальный документ целиком (санкция владельца
            // «ДА, Газ»: док компактный — финал в ответе, get не нужен)
            print_final_body(&sy, &doc_id, "персона (итог)").await;
            Ok(())
        }
        PersonSub::Replace { stdin, md } => {
            // attach_present=false: у персоны нет аттачей — ровно старая
            // семантика read_body_input (пустой контент бейлится ниже)
            let content = read_body_input(md.as_deref(), stdin, false)?;
            if content.trim().is_empty() {
                bail!(
                    "replace: пустой контент — персона не может быть пустой; правь/удаляй док в UI"
                );
            }
            let doc_id = person::replace(&sy, &target, &content).await?;
            println!(
                "персона перезаписана: {}:{} ({doc_id})",
                target.notebook, target.hpath
            );
            print_final_body(&sy, &doc_id, "персона (итог)").await;
            Ok(())
        }
    }
}
