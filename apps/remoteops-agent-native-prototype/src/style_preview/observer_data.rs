//! 演示与真实观察数据共用显示接口；真实入口只接收宿主的结构化记录和活动快照。
use super::observer_demo::{Category, DemoModel};
use remoteops_agent_desktop_host::{ActiveOperation, Observation, ObservationCategory};
use std::collections::{BTreeMap, VecDeque};

const MAX_RECORDS: usize = 2000;
// 包含记录间由表现层添加的 CRLF，保证最终全文也不超过预算。
const MAX_UTF16_UNITS: usize = 262_144;
const SHORTENED: &str = " … [后续已省略 / truncated]";

#[derive(Clone)]
struct LiveRecord {
    record_id: Option<u64>,
    // 资源准备进度按原请求原位更新。
    progress_request: Option<String>,
    // 来源来自宿主元数据，Other 仅出现在“全部”视图。
    category: Option<Category>,
    role: remoteops_agent_desktop_host::ObservationRole,
    text: String,
    units: usize,
}

#[derive(Clone, Default)]
pub(super) struct LiveHistory {
    // 唯一持久记录集合，分类只生成临时显示投影。
    records: VecDeque<LiveRecord>,
    // 完整请求标识避免同类并发任务或短号碰撞相互覆盖。
    running: BTreeMap<String, Option<Category>>,
    selected: Option<Category>,
    units: usize,
    total_received: u64,
    trimmed: u64,
    shortened: u64,
    dropped: u64,
    had_operations: bool,
}

#[derive(Clone)]
pub(super) enum ObserverData {
    Demo(DemoModel),
    Live(LiveHistory),
}
impl Default for ObserverData {
    fn default() -> Self {
        Self::Demo(DemoModel::default())
    }
}

const fn category(source: ObservationCategory) -> Option<Category> {
    match source {
        ObservationCategory::Cmd => Some(Category::Cmd),
        ObservationCategory::PowerShell => Some(Category::PowerShell),
        ObservationCategory::Shell => Some(Category::Shell),
        ObservationCategory::Ssh => Some(Category::Ssh),
        ObservationCategory::Serial => Some(Category::Serial),
        ObservationCategory::Other => None,
    }
}

// Host 已脱敏；这里只限制显示量并把 NUL 显示为可见字符，避免原生 EDIT 提前终止全文。
fn display_record(input: &str) -> (String, usize, bool) {
    let mut text = String::with_capacity(input.len().min(MAX_UTF16_UNITS * 3));
    let mut units = 0;
    let mut source = input.chars().peekable();
    let mut pending_lf = false;
    let normalized = std::iter::from_fn(move || {
        if pending_lf {
            pending_lf = false;
            return Some('\n');
        }
        let value = source.next()?;
        if value == '\r' {
            if source.peek() == Some(&'\n') {
                source.next();
            }
            pending_lf = true;
            Some('\r')
        } else if value == '\n' {
            pending_lf = true;
            Some('\r')
        } else {
            Some(value)
        }
    });
    for value in normalized {
        let value = if value == '\0' { '\u{2400}' } else { value };
        if units + value.len_utf16() > MAX_UTF16_UNITS {
            let marker_units = SHORTENED.encode_utf16().count();
            while units > MAX_UTF16_UNITS - marker_units {
                if let Some(last) = text.pop() {
                    units -= last.len_utf16();
                }
            }
            text.push_str(SHORTENED);
            units += marker_units;
            return (text, units, true);
        }
        text.push(value);
        units += value.len_utf16();
    }
    (text, units, false)
}

impl LiveHistory {
    fn append(&mut self, observation: &Observation) {
        self.had_operations |= observation.new_task;
        if observation.text.is_empty()
            && observation.role == remoteops_agent_desktop_host::ObservationRole::Info
        {
            return;
        }
        if observation.role == remoteops_agent_desktop_host::ObservationRole::Progress
            && let Some(existing) = self.records.iter_mut().rev().find(|record| {
                record.progress_request == observation.request && record.role == observation.role
            })
        {
            let (text, units, _) = display_record(&observation.text);
            self.units = self.units - existing.units + units;
            existing.text = text;
            existing.units = units;
            self.trim_to_capacity();
            return;
        }
        let record_id = observation.repetition.map(|value| value.record_id);
        if let Some(record_id) = record_id
            && let Some(existing) = self
                .records
                .iter_mut()
                .find(|record| record.record_id == Some(record_id))
        {
            let (text, units, _) = display_record(&observation.text);
            self.units = self.units - existing.units + units;
            existing.text = text;
            existing.units = units;
            self.trim_to_capacity();
            return;
        }
        self.total_received = self.total_received.saturating_add(1);
        // Host 的 Observation.text 没有附加传输换行；结尾空行属于正文，必须保留。
        let (text, units, shortened) = display_record(&observation.text);
        if shortened {
            self.shortened = self.shortened.saturating_add(1);
        }
        if !self.records.is_empty() {
            self.units += 2;
        }
        self.units += units;
        self.records.push_back(LiveRecord {
            record_id,
            progress_request: (observation.role
                == remoteops_agent_desktop_host::ObservationRole::Progress)
                .then(|| observation.request.clone())
                .flatten(),
            category: category(observation.category),
            role: observation.role,
            text,
            units,
        });
        self.trim_to_capacity();
    }

    fn trim_to_capacity(&mut self) {
        while self.records.len() > MAX_RECORDS || self.units > MAX_UTF16_UNITS {
            if let Some(oldest) = self.records.pop_front() {
                self.units -= oldest.units;
                if !self.records.is_empty() {
                    self.units -= 2;
                }
                self.trimmed = self.trimmed.saturating_add(1);
            }
        }
    }

    fn status(&self, english: bool) -> String {
        let total = self.running.len();
        let outside = self.selected.map_or(0, |selected| {
            total.saturating_sub(
                self.running
                    .values()
                    .filter(|value| **value == Some(selected))
                    .count(),
            )
        });
        let mut parts = vec![if total > 0 {
            if english {
                format!("{total} operations running")
            } else {
                format!("正在执行 {total} 项操作")
            }
        } else if self.had_operations {
            if english {
                "Previous operations ended · none running".into()
            } else {
                "当前操作已结束".into()
            }
        } else if english {
            "No operations yet".into()
        } else {
            "尚未执行操作".into()
        }];
        if outside > 0 {
            parts.push(if english {
                format!("{outside} in other categories")
            } else {
                format!("其他分类 {outside} 项")
            });
        }
        if self.trimmed > 0 {
            let count = self.trimmed;
            parts.push(if english {
                format!("{count} old records trimmed")
            } else {
                format!("历史已裁剪 {count} 条")
            });
        }
        if self.shortened > 0 {
            let count = self.shortened;
            parts.push(if english {
                format!("{count} long records shortened")
            } else {
                format!("长记录已省略 {count} 条")
            });
        }
        if self.dropped > 0 {
            let count = self.dropped;
            parts.push(if english {
                format!("{count} intermediate display records omitted")
            } else {
                format!("中间显示记录已省略 {count} 条")
            });
        }
        parts.join(" · ")
    }
}

impl ObserverData {
    pub(super) fn live() -> Self {
        Self::Live(LiveHistory::default())
    }

    pub(super) fn rows(&self) -> Vec<String> {
        match self {
            Self::Demo(model) => model.rows(),
            Self::Live(history) => history
                .records
                .iter()
                .filter(|record| history.selected.is_none() || record.category == history.selected)
                .map(|record| record.text.clone())
                .collect(),
        }
    }

    pub(super) fn line_roles(&self) -> Vec<remoteops_agent_desktop_host::ObservationRole> {
        use remoteops_agent_desktop_host::ObservationRole;
        match self {
            Self::Demo(model) => model
                .rows()
                .iter()
                .flat_map(|row| {
                    std::iter::repeat_n(ObservationRole::Output, row.matches('\n').count() + 1)
                })
                .collect(),
            Self::Live(history) => history
                .records
                .iter()
                .filter(|record| history.selected.is_none() || record.category == history.selected)
                .flat_map(|record| {
                    std::iter::repeat_n(record.role, record.text.matches('\n').count() + 1)
                })
                .collect(),
        }
    }

    pub(super) fn all_rows(&self) -> Vec<String> {
        match self {
            Self::Demo(model) => model.all_rows(),
            Self::Live(history) => history
                .records
                .iter()
                .map(|record| record.text.clone())
                .collect(),
        }
    }

    pub(super) fn total_count(&self) -> usize {
        match self {
            Self::Demo(model) => model.total_count(),
            Self::Live(history) => history.records.len(),
        }
    }

    pub(super) fn visible_count(&self) -> usize {
        match self {
            Self::Demo(model) => model.visible_count(),
            Self::Live(history) => history
                .records
                .iter()
                .filter(|record| history.selected.is_none() || record.category == history.selected)
                .count(),
        }
    }

    pub(super) const fn selected(&self) -> Option<Category> {
        match self {
            Self::Demo(model) => model.selected(),
            Self::Live(history) => history.selected,
        }
    }

    pub(super) fn select(&mut self, selected: Option<Category>) {
        match self {
            Self::Demo(model) => model.select(selected),
            Self::Live(history) => history.selected = selected,
        }
    }

    pub(super) fn running_count(&self, selected: Option<Category>) -> usize {
        match self {
            Self::Demo(model) => model.running_count(selected),
            Self::Live(history) => history
                .running
                .values()
                .filter(|value| selected.is_none_or(|selected| **value == Some(selected)))
                .count(),
        }
    }

    pub(super) fn total_running(&self) -> usize {
        match self {
            Self::Demo(model) => model.total_running(),
            Self::Live(history) => history.running.len(),
        }
    }

    pub(super) fn status(&self, english: bool) -> String {
        match self {
            Self::Demo(model) => model.status(english),
            Self::Live(history) => history.status(english),
        }
    }

    pub(super) fn tick(&mut self) -> bool {
        match self {
            Self::Demo(model) => model.tick(),
            Self::Live(_) => false,
        }
    }

    pub(super) fn start_scene(&mut self, scene: usize) {
        if let Self::Demo(model) = self {
            model.start_scene(scene);
        }
    }

    pub(super) fn start_concurrent(&mut self) {
        if let Self::Demo(model) = self {
            model.start_concurrent();
        }
    }

    pub(super) fn finish_all(&mut self) {
        if let Self::Demo(model) = self {
            model.finish_all();
        }
    }

    pub(super) fn replace_large_demo(&mut self, rows: Vec<String>) {
        if let Self::Demo(model) = self {
            model.replace_large_demo(rows);
        }
    }

    pub(super) fn ingest(
        &mut self,
        observations: Vec<Observation>,
        active: &[ActiveOperation],
    ) -> bool {
        let Self::Live(history) = self else {
            return false;
        };
        let running: BTreeMap<_, _> = active
            .iter()
            .map(|operation| (operation.request.clone(), category(operation.category)))
            .collect();
        let changed = !observations.is_empty() || running != history.running;
        history.had_operations |= !running.is_empty();
        history.running = running;
        for observation in observations {
            history.append(&observation);
        }
        changed
    }

    pub(super) fn set_dropped(&mut self, dropped: u64) -> bool {
        let Self::Live(history) = self else {
            return false;
        };
        let changed = history.dropped != dropped;
        history.dropped = dropped;
        changed
    }

    pub(super) const fn trimmed_count(&self) -> u64 {
        match self {
            Self::Demo(_) => 0,
            Self::Live(history) => history.trimmed,
        }
    }

    pub(super) fn total_received(&self) -> u64 {
        match self {
            Self::Demo(model) => u64::try_from(model.total_count()).unwrap_or(u64::MAX),
            Self::Live(history) => history.total_received,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn observation(category: ObservationCategory, text: &str) -> Observation {
        Observation {
            repetition: None,
            text: text.into(),
            role: remoteops_agent_desktop_host::ObservationRole::Output,
            category,
            request: Some("request-fixture".into()),
            new_task: false,
        }
    }

    #[test]
    fn cumulative_serial_updates_replace_records_and_keep_categories() {
        let mut model = ObserverData::live();
        for total in [1, 128, 10_000] {
            let mut record = observation(ObservationCategory::Serial, &format!("same × {total}"));
            record.repetition = Some(remoteops_agent_desktop_host::ObservationRepetition {
                record_id: 42,
                total,
            });
            model.ingest(vec![record], &[]);
        }
        assert_eq!(model.rows(), ["same × 10000"]);
        assert_eq!(model.total_received(), 1);
        assert_eq!(model.total_count(), 1);
        model.select(Some(Category::Cmd));
        assert_eq!(model.rows(), Vec::<String>::new());
        model.select(Some(Category::Serial));
        assert_eq!(model.rows(), ["same × 10000"]);
    }

    #[test]
    fn terminal_newlines_normalize_without_inserting_extra_blank_lines() {
        assert_eq!(
            display_record("a\r\nb\nc\rd\n\n").0,
            "a\r\nb\r\nc\r\nd\r\n\r\n"
        );
    }

    #[test]
    fn structured_roles_follow_multiline_records_and_category_projection() {
        use remoteops_agent_desktop_host::ObservationRole;
        let mut model = ObserverData::live();
        let mut failure = observation(ObservationCategory::Cmd, "错误\r\n下一行");
        failure.role = ObservationRole::Error;
        let mut success = observation(ObservationCategory::PowerShell, "成功");
        success.role = ObservationRole::Success;
        model.ingest(vec![failure, success], &[]);
        assert_eq!(
            model.line_roles(),
            vec![
                ObservationRole::Error,
                ObservationRole::Error,
                ObservationRole::Success
            ]
        );
        model.select(Some(Category::PowerShell));
        assert_eq!(model.line_roles(), vec![ObservationRole::Success]);
        model.select(Some(Category::Shell));
        assert_eq!(
            model.line_roles(),
            Vec::<remoteops_agent_desktop_host::ObservationRole>::new()
        );
    }

    fn active(request: &str, category: ObservationCategory) -> ActiveOperation {
        ActiveOperation {
            request: request.into(),
            category,
        }
    }

    #[test]
    fn live_starts_empty_and_never_invents_demo_output() {
        let mut model = ObserverData::live();
        assert_eq!(model.rows(), Vec::<String>::new());
        assert_eq!(model.total_count(), 0);
        assert_eq!(model.total_running(), 0);
        assert_eq!(model.status(false), "尚未执行操作");
        model.start_scene(0);
        model.start_concurrent();
        model.finish_all();
        model.replace_large_demo(vec!["synthetic".into()]);
        assert!(!model.tick());
        assert_eq!(model.rows(), Vec::<String>::new());
        assert_eq!(model.total_received(), 0);
    }

    #[test]
    fn demo_keeps_legacy_methods_and_does_not_ingest_business_data() {
        let mut model = ObserverData::default();
        assert_eq!(model.total_count(), 6);
        let original = model.all_rows();
        assert!(!model.ingest(
            vec![observation(ObservationCategory::Cmd, "real")],
            &[active("cmd", ObservationCategory::Cmd)]
        ));
        assert_eq!(model.all_rows(), original);
        model.start_concurrent();
        assert_eq!(model.total_running(), 2);
        assert!(model.tick());
        model.finish_all();
        assert_eq!(model.total_running(), 0);
        model.replace_large_demo(vec!["large".into()]);
        assert_eq!(model.all_rows(), vec!["large"]);
    }

    #[test]
    fn real_activity_uses_unique_full_request_ids_and_structured_categories() {
        let mut model = ObserverData::live();
        let active = [
            active("sameprefix-cmd", ObservationCategory::Cmd),
            active("sameprefix-ps", ObservationCategory::PowerShell),
            active("ssh-one", ObservationCategory::Ssh),
            active("ssh-two", ObservationCategory::Ssh),
        ];
        assert!(model.ingest(Vec::new(), &active));
        assert_eq!(model.total_running(), 4);
        assert_eq!(model.running_count(Some(Category::Cmd)), 1);
        assert_eq!(model.running_count(Some(Category::PowerShell)), 1);
        assert_eq!(model.running_count(Some(Category::Ssh)), 2);
        assert!(!model.ingest(Vec::new(), &active));
        let same_count_new_ids = [
            active[0].clone(),
            active[1].clone(),
            active[2].clone(),
            ActiveOperation {
                request: "ssh-three".into(),
                category: ObservationCategory::Ssh,
            },
        ];
        assert!(model.ingest(Vec::new(), &same_count_new_ids));
        assert_eq!(model.total_running(), 4);
    }

    #[test]
    fn filter_order_and_selection_survive_other_categories_and_completion() {
        let mut model = ObserverData::live();
        model.select(Some(Category::PowerShell));
        let active = [
            active("ssh-one", ObservationCategory::Ssh),
            active("ssh-two", ObservationCategory::Ssh),
        ];
        model.ingest(
            vec![
                observation(ObservationCategory::Cmd, "CMD mentions PowerShell and SSH"),
                observation(ObservationCategory::PowerShell, "PS first"),
                observation(ObservationCategory::Ssh, "ssh"),
                observation(ObservationCategory::Other, "system"),
                observation(ObservationCategory::PowerShell, "PS second"),
            ],
            &active,
        );
        assert_eq!(model.rows(), vec!["PS first", "PS second"]);
        assert_eq!(model.selected(), Some(Category::PowerShell));
        assert_eq!(model.running_count(Some(Category::PowerShell)), 0);
        assert!(model.status(false).contains("其他分类 2 项"));
        assert!(model.ingest(Vec::new(), &[]));
        assert_eq!(model.total_running(), 0);
        assert_eq!(model.selected(), Some(Category::PowerShell));
        assert_eq!(model.rows(), vec!["PS first", "PS second"]);
        model.select(None);
        assert_eq!(model.total_count(), 5);
        assert_eq!(model.all_rows()[3], "system");
    }

    #[test]
    fn records_and_demo_controls_cannot_start_or_finish_real_operations() {
        let mut model = ObserverData::live();
        model.ingest(
            vec![observation(
                ObservationCategory::Cmd,
                "Running 成功 完成 SSH",
            )],
            &[active("request", ObservationCategory::Cmd)],
        );
        model.finish_all();
        assert_eq!(model.total_running(), 1);
        model.start_concurrent();
        model.start_scene(1);
        assert_eq!(model.total_running(), 1);
        model.ingest(vec![observation(ObservationCategory::Cmd, "仍在执行")], &[]);
        assert_eq!(model.total_running(), 0);
        assert!(model.status(false).contains("已结束"));
        assert!(!model.status(false).contains("演示"));
    }

    #[test]
    fn unknown_operations_are_visible_only_in_all_but_count_as_real_activity() {
        let mut model = ObserverData::live();
        model.ingest(
            vec![observation(ObservationCategory::Other, "CMD PowerShell")],
            &[active("other", ObservationCategory::Other)],
        );
        assert_eq!(model.total_running(), 1);
        assert_eq!(model.running_count(Some(Category::Cmd)), 0);
        model.select(Some(Category::Cmd));
        assert_eq!(model.rows(), Vec::<String>::new());
        model.select(None);
        assert_eq!(model.rows(), vec!["CMD PowerShell"]);
    }

    #[test]
    fn history_record_limit_does_not_trim_activity_or_change_selection() {
        let mut model = ObserverData::live();
        model.select(Some(Category::Cmd));
        let observations = (0..=MAX_RECORDS)
            .map(|index| observation(ObservationCategory::Cmd, &index.to_string()))
            .collect();
        model.ingest(observations, &[active("running", ObservationCategory::Cmd)]);
        assert_eq!(model.total_count(), MAX_RECORDS);
        assert_eq!(model.trimmed_count(), 1);
        assert_eq!(
            model.total_received(),
            u64::try_from(MAX_RECORDS + 1).unwrap()
        );
        assert_eq!(model.rows().first().map(String::as_str), Some("1"));
        assert_eq!(model.total_running(), 1);
        assert_eq!(model.selected(), Some(Category::Cmd));
    }

    #[test]
    fn utf16_budget_includes_crlf_and_never_splits_emoji() {
        let mut model = ObserverData::live();
        let huge = "😀".repeat(MAX_UTF16_UNITS);
        model.ingest(
            vec![observation(ObservationCategory::Cmd, &huge)],
            &[active("running", ObservationCategory::Cmd)],
        );
        let text = model.all_rows().join("\r\n");
        assert!(text.ends_with(SHORTENED));
        assert!(text.encode_utf16().count() <= MAX_UTF16_UNITS);
        assert!(!text.contains('\u{fffd}'));
        model.ingest(
            vec![observation(ObservationCategory::Cmd, "tail")],
            &[active("running", ObservationCategory::Cmd)],
        );
        assert!(model.all_rows().join("\r\n").encode_utf16().count() <= MAX_UTF16_UNITS);
        assert_eq!(model.total_running(), 1);
        assert_eq!(model.total_received(), 2);
        assert!(model.trimmed_count() > 0);
    }

    #[test]
    fn record_separators_do_not_remove_real_blank_lines_or_nul_tail() {
        let mut model = ObserverData::live();
        model.ingest(
            vec![
                observation(ObservationCategory::Cmd, "first\r\n\r\n"),
                observation(ObservationCategory::Cmd, ""),
                observation(ObservationCategory::Cmd, "a\0b"),
            ],
            &[],
        );
        assert_eq!(model.all_rows(), vec!["first\r\n\r\n", "", "a␀b"]);
        assert_eq!(model.total_count(), 3);
    }

    #[test]
    fn omitted_records_are_reported_without_changing_tasks_or_history() {
        let mut model = ObserverData::live();
        assert!(model.set_dropped(12));
        assert!(!model.set_dropped(12));
        assert!(model.status(false).contains("省略 12 条"));
        assert!(
            model
                .status(true)
                .contains("12 intermediate display records omitted")
        );
        assert_eq!(model.total_count(), 0);
        assert_eq!(model.total_running(), 0);
        assert!(model.set_dropped(0));
        assert_eq!(model.status(false), "尚未执行操作");
    }
}
