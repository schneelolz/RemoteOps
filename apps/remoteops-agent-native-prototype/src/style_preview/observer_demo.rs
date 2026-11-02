//! 观察窗的合成事件模型：分类和活动状态由显式事件决定，不解析正文或执行命令。
use std::collections::{BTreeMap, VecDeque};

// 普通交互仅保留最近记录；显式大日志场景单独允许更大的压力样本。
const MAX_RECORDS: usize = 200;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) enum Category {
    Cmd,
    PowerShell,
    Shell,
    Ssh,
    Serial,
}
impl Category {
    pub(super) const ALL: [Self; 5] = [
        Self::Cmd,
        Self::PowerShell,
        Self::Shell,
        Self::Ssh,
        Self::Serial,
    ];

    pub(super) const fn label(self, english: bool) -> &'static str {
        match self {
            Self::Cmd => "CMD",
            Self::PowerShell => "PowerShell",
            Self::Shell => "Shell",
            Self::Ssh => "SSH",
            Self::Serial if english => "Serial",
            Self::Serial => "串口",
        }
    }
}

#[derive(Clone)]
struct Record {
    // 来源独立于正文；命令提到其他解释器时不会改变归类。
    category: Option<Category>,
    text: String,
}

#[derive(Clone)]
enum Action {
    Start {
        task: u64,
        category: Category,
        command: String,
    },
    Line {
        category: Option<Category>,
        text: String,
    },
    Finish {
        task: u64,
        detail: String,
    },
}

#[derive(Clone)]
struct Scheduled {
    // 一个 tick 对应一秒演示时间，不依赖真实后台工作或输出频率。
    at: u32,
    action: Action,
}

#[derive(Clone)]
pub(super) struct DemoModel {
    // 所有分类共享唯一记录集合，筛选只创建显示投影。
    records: Vec<Record>,
    // 每项活动均有独立短号，同一类别可以同时运行多个任务。
    running: BTreeMap<u64, Category>,
    // 演示步骤与显示暂停无关，由宿主按秒推进。
    scheduled: VecDeque<Scheduled>,
    elapsed: u32,
    next_task: u64,
    selected: Option<Category>,
    completed: usize,
    started: bool,
}

impl Default for DemoModel {
    fn default() -> Self {
        let records = [
            (None, "[界面演示] 以下内容均为合成数据；本窗口只读。"),
            (Some(Category::Cmd), "[CMD] > echo 中文输出正常"),
            (Some(Category::Cmd), "中文输出正常。"),
            (
                Some(Category::Ssh),
                "[SSH] > plink.exe -ssh demo@192.0.2.10 -pw [已隐藏]",
            ),
            (
                Some(Category::Ssh),
                "[SSH] > display version → 演示交换机 v1.0",
            ),
            (
                Some(Category::Serial),
                "[串口 Console] COM3 · 115200 / 8N1 → GE0/0/1 UP",
            ),
        ]
        .into_iter()
        .map(|(category, text)| Record {
            category,
            text: text.into(),
        })
        .collect();
        Self {
            records,
            running: BTreeMap::new(),
            scheduled: VecDeque::new(),
            elapsed: 0,
            next_task: 1,
            selected: None,
            completed: 0,
            started: false,
        }
    }
}

impl DemoModel {
    pub(super) fn rows(&self) -> Vec<String> {
        self.records
            .iter()
            .filter(|record| self.selected.is_none() || record.category == self.selected)
            .map(|record| record.text.clone())
            .collect()
    }

    pub(super) fn all_rows(&self) -> Vec<String> {
        self.records
            .iter()
            .map(|record| record.text.clone())
            .collect()
    }

    pub(super) fn total_count(&self) -> usize {
        self.records.len()
    }

    pub(super) fn visible_count(&self) -> usize {
        self.records
            .iter()
            .filter(|record| self.selected.is_none() || record.category == self.selected)
            .count()
    }

    pub(super) const fn selected(&self) -> Option<Category> {
        self.selected
    }

    pub(super) fn select(&mut self, category: Option<Category>) {
        self.selected = category;
    }

    pub(super) fn running_count(&self, category: Option<Category>) -> usize {
        self.running
            .values()
            .filter(|running| category.is_none_or(|category| **running == category))
            .count()
    }

    pub(super) fn total_running(&self) -> usize {
        self.running.len()
    }

    pub(super) fn status(&self, english: bool) -> String {
        if !self.running.is_empty() {
            let total = self.total_running();
            let outside = self.selected.map_or(0, |selected| {
                total.saturating_sub(self.running_count(Some(selected)))
            });
            if english {
                if outside == 0 {
                    format!("Demo running · {total} active")
                } else {
                    format!("Demo running · {total} active · {outside} in other categories")
                }
            } else if outside == 0 {
                format!("界面演示执行中 · {total} 项")
            } else {
                format!("界面演示执行中 · {total} 项 · 其他分类 {outside} 项")
            }
        } else if !self.scheduled.is_empty() {
            if english {
                "Demo waiting for next step".into()
            } else {
                "界面演示等待下一步".into()
            }
        } else if self.started {
            if english {
                format!("Demo ended · {} completed", self.completed)
            } else {
                format!("本轮演示已结束 · 完成 {} 项", self.completed)
            }
        } else if english {
            "Demo ready · no active operations".into()
        } else {
            "演示就绪 · 当前没有执行中的操作".into()
        }
    }

    fn append(&mut self, category: Option<Category>, text: String) {
        self.records.push(Record { category, text });
        if self.records.len() > MAX_RECORDS {
            self.records.drain(..self.records.len() - MAX_RECORDS);
        }
    }

    fn allocate(&mut self) -> u64 {
        let task = self.next_task;
        self.next_task = self.next_task.saturating_add(1);
        task
    }

    fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::Start {
                task,
                category,
                command,
            } => {
                self.running.insert(task, category);
                self.append(
                    Some(category),
                    format!("[{} #D{task:04}] > {command}", category.label(false)),
                );
                true
            }
            Action::Line { category, text } => {
                self.append(category, text);
                true
            }
            Action::Finish { task, detail } => {
                if let Some(category) = self.running.remove(&task) {
                    self.append(
                        Some(category),
                        format!("[{} #D{task:04}] [完成] {detail}", category.label(false)),
                    );
                    self.completed += 1;
                    true
                } else {
                    false
                }
            }
        }
    }

    fn schedule(&mut self, at: u32, action: Action) {
        self.scheduled.push_back(Scheduled { at, action });
    }

    fn line(&mut self, at: u32, task: u64, category: Category, text: &str) {
        self.schedule(
            at,
            Action::Line {
                category: Some(category),
                text: format!("[{} #D{task:04}] {text}", category.label(false)),
            },
        );
    }

    fn finish(&mut self, at: u32, task: u64, detail: &str) {
        self.schedule(
            at,
            Action::Finish {
                task,
                detail: detail.into(),
            },
        );
    }

    fn begin_round(&mut self) {
        // 再次演示前给旧活动补结束记录，不留下亮点或悄悄抹掉历史。
        self.finish_all();
        self.elapsed = 0;
        self.completed = 0;
        self.started = true;
    }

    pub(super) fn start_scene(&mut self, scene: usize) {
        self.begin_round();
        let task = self.allocate();
        let (category, command, lines): (_, _, &[&str]) = match scene {
            0 => (
                Category::Cmd,
                "ver",
                &[
                    "Microsoft Windows [版本 10.0.26100]",
                    "> echo 中文路径：C:\\演示 文件\\日志",
                    "中文路径显示正常。",
                ],
            ),
            1 => (
                Category::Ssh,
                "plink.exe -ssh demo@192.0.2.10 -pw [已隐藏]",
                &[
                    "正在验证已固定的主机密钥…",
                    "认证成功（界面演示）",
                    "> display version",
                    "演示交换机 · 软件版本 1.0",
                ],
            ),
            _ => (
                Category::Serial,
                "打开 COM3 · 115200 / 8N1",
                &[
                    "> display interface brief",
                    "GE0/0/1  UP    1000M",
                    "GE0/0/2  DOWN  --",
                ],
            ),
        };
        self.apply(Action::Start {
            task,
            category,
            command: command.into(),
        });
        for (index, text) in lines.iter().enumerate() {
            self.line(
                u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1),
                task,
                category,
                text,
            );
        }
        self.finish(6, task, "合成操作已结束，未执行真实命令");
    }

    pub(super) fn start_concurrent(&mut self) {
        self.begin_round();
        self.append(
            None,
            "[界面演示] 并发场景开始；只模拟状态，不执行真实命令。".into(),
        );
        let cmd = self.allocate();
        let powershell = self.allocate();
        let ssh_a = self.allocate();
        let ssh_b = self.allocate();
        let shell = self.allocate();
        let serial = self.allocate();
        for (task, category, command) in [
            (cmd, Category::Cmd, "echo 中文输出正常 & ver"),
            (
                powershell,
                Category::PowerShell,
                "Get-Date; Get-Process | Select-Object -First 3",
            ),
        ] {
            self.apply(Action::Start {
                task,
                category,
                command: command.into(),
            });
        }
        self.line(1, cmd, Category::Cmd, "中文输出正常。");
        self.line(2, powershell, Category::PowerShell, "2026-11-01 · 合成时间");
        self.line(3, cmd, Category::Cmd, "Microsoft Windows · 合成版本");
        self.finish(5, cmd, "CMD 演示结束 · 退出码 0（合成）");
        for (task, target) in [(ssh_a, "192.0.2.10"), (ssh_b, "192.0.2.11")] {
            self.schedule(
                6,
                Action::Start {
                    task,
                    category: Category::Ssh,
                    command: format!("plink.exe -ssh demo@{target} -pw [已隐藏]"),
                },
            );
        }
        self.line(
            7,
            ssh_a,
            Category::Ssh,
            "主机密钥核对通过；认证成功（合成）",
        );
        self.line(
            8,
            ssh_b,
            Category::Ssh,
            "认证信息已隐藏；读取交换机状态（合成）",
        );
        self.finish(9, powershell, "PowerShell 演示结束 · 退出码 0（合成）");
        self.line(
            10,
            ssh_a,
            Category::Ssh,
            "display version → 演示交换机 v1.0",
        );
        self.line(
            11,
            ssh_b,
            Category::Ssh,
            "display interface brief → GE0/0/1 UP",
        );
        self.schedule(
            12,
            Action::Start {
                task: shell,
                category: Category::Shell,
                command: "/bin/sh -c 'printf 中文输出正常'（合成）".into(),
            },
        );
        self.line(
            13,
            shell,
            Category::Shell,
            "实际解释器示例：/bin/sh；中文输出正常。",
        );
        self.schedule(
            14,
            Action::Start {
                task: serial,
                category: Category::Serial,
                command: "COM3 · 115200 / 8N1 → display interface brief（合成）".into(),
            },
        );
        self.line(15, serial, Category::Serial, "GE0/0/1 UP · 合成串口输出");
        self.finish(16, ssh_a, "SSH 演示任务 A 已结束");
        self.finish(18, ssh_b, "SSH 演示任务 B 已结束");
        self.finish(20, shell, "Shell 演示结束 · 退出码 0（合成）");
        self.finish(25, serial, "串口演示结束；本轮所有活动已完成");
    }

    /// 宿主每个演示秒调用一次；显示暂停只阻止视图追随，不阻止此处状态前进。
    pub(super) fn tick(&mut self) -> bool {
        if self.scheduled.is_empty() {
            return false;
        }
        self.elapsed = self.elapsed.saturating_add(1);
        let mut changed = false;
        while self
            .scheduled
            .front()
            .is_some_and(|step| step.at <= self.elapsed)
        {
            if let Some(step) = self.scheduled.pop_front() {
                changed |= self.apply(step.action);
            }
        }
        changed
    }

    pub(super) fn finish_all(&mut self) {
        self.scheduled.clear();
        let tasks: Vec<_> = self.running.keys().copied().collect();
        for task in tasks {
            self.apply(Action::Finish {
                task,
                detail: "合成任务已结束（手动演示）".into(),
            });
        }
    }

    pub(super) fn replace_large_demo(&mut self, rows: Vec<String>) {
        self.records = rows
            .into_iter()
            .map(|text| Record {
                category: Some(Category::Shell),
                text,
            })
            .collect();
        self.running.clear();
        self.scheduled.clear();
        self.selected = None;
        self.elapsed = 0;
        self.completed = 0;
        self.started = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_rows_preserve_legacy_text_and_have_no_active_task() {
        let model = DemoModel::default();
        assert_eq!(model.total_count(), 6);
        assert_eq!(model.visible_count(), 6);
        assert_eq!(model.total_running(), 0);
        assert_eq!(model.rows()[2], "中文输出正常。");
        assert!(model.rows()[3].ends_with("-pw [已隐藏]"));
    }

    #[test]
    fn filters_preserve_order_without_changing_shared_history() {
        let mut model = DemoModel::default();
        let all = model.all_rows();
        model.select(Some(Category::Cmd));
        assert_eq!(model.rows(), all[1..3]);
        assert_eq!(model.visible_count(), 2);
        assert_eq!(model.total_count(), all.len());
        model.select(Some(Category::Ssh));
        assert_eq!(model.rows(), all[3..5]);
        model.select(None);
        assert_eq!(model.rows(), all);
    }

    #[test]
    fn classification_uses_explicit_source_instead_of_command_text() {
        let mut model = DemoModel::default();
        model.append(Some(Category::Ssh), "[CMD] PowerShell bash plink".into());
        model.select(Some(Category::Ssh));
        assert_eq!(model.rows().last().unwrap(), "[CMD] PowerShell bash plink");
        model.select(Some(Category::Cmd));
        assert!(!model.rows().iter().any(|row| row.contains("bash")));
    }

    #[test]
    fn empty_filter_does_not_invent_output_or_activity() {
        let mut model = DemoModel::default();
        model.select(Some(Category::PowerShell));
        assert!(model.rows().is_empty());
        assert_eq!(model.visible_count(), 0);
        assert_eq!(model.running_count(model.selected()), 0);
    }

    #[test]
    fn concurrent_timeline_has_multiple_categories_and_two_ssh_tasks() {
        let mut model = DemoModel::default();
        model.start_concurrent();
        assert_eq!(model.running_count(Some(Category::Cmd)), 1);
        assert_eq!(model.running_count(Some(Category::PowerShell)), 1);
        assert_eq!(model.total_running(), 2);
        for _ in 0..4 {
            model.tick();
        }
        assert_eq!(model.running_count(Some(Category::Cmd)), 1);
        for _ in 4..6 {
            model.tick();
        }
        assert_eq!(model.running_count(Some(Category::Cmd)), 0);
        assert_eq!(model.running_count(Some(Category::Ssh)), 2);
        assert_eq!(model.total_running(), 3);
        assert!(model.status(false).contains("执行中"));
    }

    #[test]
    fn selected_view_stays_selected_as_other_categories_run_and_finish() {
        let mut model = DemoModel::default();
        model.select(Some(Category::Cmd));
        model.start_concurrent();
        for _ in 0..8 {
            model.tick();
        }
        assert_eq!(model.selected(), Some(Category::Cmd));
        assert!(model.status(false).contains("其他分类 3 项"));
        assert!(model.rows().iter().all(|row| !row.contains("[SSH")));
    }

    #[test]
    fn full_timeline_finishes_every_task_without_lingering_activity() {
        let mut model = DemoModel::default();
        model.start_concurrent();
        for _ in 0..25 {
            model.tick();
        }
        assert_eq!(model.total_running(), 0);
        assert!(model.scheduled.is_empty());
        assert_eq!(model.completed, 6);
        assert!(model.status(false).contains("已结束"));
        assert!(model.status(true).contains("6 completed"));
        assert!(!model.tick());
    }

    #[test]
    fn manual_finish_preserves_history_and_prevents_late_steps() {
        let mut model = DemoModel::default();
        model.start_concurrent();
        let before = model.all_rows();
        model.finish_all();
        assert!(model.all_rows().starts_with(&before));
        assert_eq!(model.total_running(), 0);
        assert_eq!(model.completed, 2);
        let completed = model.all_rows();
        assert!(!model.tick());
        assert_eq!(model.all_rows(), completed);
    }

    #[test]
    fn restarting_demo_finishes_old_tasks_and_uses_new_short_ids() {
        let mut model = DemoModel::default();
        model.start_concurrent();
        model.start_scene(1);
        assert_eq!(model.total_running(), 1);
        assert_eq!(model.running_count(Some(Category::Ssh)), 1);
        assert!(model.rows().last().unwrap().contains("#D0007"));
        for _ in 0..6 {
            model.tick();
        }
        assert_eq!(model.total_running(), 0);
    }

    #[test]
    fn large_demo_keeps_exact_rows_and_clears_old_activity() {
        let mut model = DemoModel::default();
        model.select(Some(Category::Cmd));
        model.start_concurrent();
        let rows: Vec<_> = (0..10_001)
            .map(|index| format!("{index:05} 中文 压力输出"))
            .collect();
        model.replace_large_demo(rows.clone());
        assert_eq!(model.rows(), rows);
        assert_eq!(model.total_count(), 10_001);
        assert_eq!(model.selected(), None);
        assert_eq!(model.total_running(), 0);
        assert!(!model.tick());
        model.select(Some(Category::Shell));
        assert_eq!(model.visible_count(), 10_001);
        model.select(Some(Category::Cmd));
        assert_eq!(model.visible_count(), 0);
    }

    #[test]
    fn repeated_scenes_keep_history_bounded_without_changing_task_state() {
        let mut model = DemoModel::default();
        for _ in 0..100 {
            model.start_concurrent();
            for _ in 0..6 {
                model.tick();
            }
            assert!(model.total_count() <= MAX_RECORDS);
            assert_eq!(model.running_count(Some(Category::Ssh)), 2);
            assert_eq!(model.total_running(), 3);
        }
        model.finish_all();
        assert_eq!(model.total_running(), 0);
        assert!(model.total_count() <= MAX_RECORDS);
        assert!(model.rows().last().unwrap().contains("[完成]"));
    }

    #[test]
    fn first_normal_append_after_large_demo_converges_to_history_limit() {
        let mut model = DemoModel::default();
        model.replace_large_demo(
            (0..10_001)
                .map(|index| format!("pressure-{index}"))
                .collect(),
        );
        assert_eq!(model.total_count(), 10_001);
        model.start_scene(0);
        assert_eq!(model.total_count(), MAX_RECORDS);
        assert_eq!(model.total_running(), 1);
        assert_eq!(model.running_count(Some(Category::Cmd)), 1);
        assert_eq!(model.rows()[0], "pressure-9802");
        assert!(model.rows().last().unwrap().contains("[CMD"));
    }
}
