//! 发版要求（只在 --release 时检查）：从提交过的代码构建，报告里记下的提交就是发出去的版本；
//! 许可证全文随安装包发布。

use std::path::Path;

use crate::report::Outcome;
use crate::{bundle, util};

pub fn run(root: &Path) -> Outcome {
    let mut problems = Vec::new();
    let head = util::git_head(root);
    match &head {
        None => problems.push("还没有提交：发版要从提交过的代码构建，报告里才对得上版本".to_string()),
        Some(_) if util::git_dirty(root) => problems.push("工作区有未提交的改动：提交之后再跑一遍".into()),
        Some(_) => {}
    }
    if !root.join("LICENSE").is_file() {
        problems.push("仓库根目录没有 LICENSE：项目和雾凇词库都是 GPL-3.0，发布时要附上许可证全文".into());
    } else if !root.join(bundle::APP).join("Contents/Resources/LICENSE").is_file() {
        problems.push("Kyme.app 里没有 LICENSE：先跑安装包检查，scripts/build-app.sh 会把它放进去".into());
    }
    match head {
        Some(head) if problems.is_empty() => Outcome::pass(format!("提交 {head}，LICENSE 已随安装包")),
        _ => Outcome::fail(format!("{} 项没达到", problems.len())).with_details(problems),
    }
}
