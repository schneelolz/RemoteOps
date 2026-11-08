//! 仅在已授权资源实例内执行的网页秘密填写；上游原始响应不向控制端透传。
use super::*;
use zeroize::Zeroizing;

pub(super) struct PasswordBinding {
    id: String,
    origin: String,
    selector: String,
    expires: std::time::Instant,
}

impl ResourceManager {
    /// 核验当前主文档的唯一密码框，返回单次目标绑定。
    /// # Errors
    /// 拒绝错误实例、站点、非密码字段及不支持的浏览器。
    pub async fn prepare_browser_password(
        &self,
        session: &str,
        instance: &str,
        origin: &str,
        selector: &str,
        username: &str,
    ) -> Result<Value> {
        self.call(
            session,
            instance,
            "__remoteops_prepare_password",
            json!({"origin":origin,"selector":selector,"username":username}),
        )
        .await
    }

    /// 向已核验的单次目标填写密码；只返回固定状态。
    /// # Errors
    /// 拒绝过期、变化、跨会话、重复绑定及调用结果未知。
    pub async fn fill_browser_password(
        &self,
        session: &str,
        instance: &str,
        binding: &str,
        password: &str,
    ) -> Result<Value> {
        self.call(
            session,
            instance,
            "__remoteops_fill_password",
            json!({"binding":binding,"password":password}),
        )
        .await
    }
}

impl Instance {
    pub(super) async fn password_operation(
        &mut self,
        name: &str,
        mut args: Value,
    ) -> Result<Value> {
        let result = if name == "__remoteops_prepare_password" {
            self.prepare_password(&args).await
        } else {
            let password = Zeroizing::new(args["password"].as_str().unwrap_or_default().to_owned());
            if let Some(Value::String(value)) = args.get_mut("password") {
                use zeroize::Zeroize as _;
                value.zeroize();
            }
            self.fill_password(args["binding"].as_str().unwrap_or_default(), password)
                .await
        };
        // 不透传浏览器原始异常，其中可能含脚本或输入值。
        result.map_err(|error| {
            if error.retry_safe {
                ResourceError::before(
                    "browser_password_target_changed",
                    "密码目标无效、已变化、过期或浏览器适配不支持；未自动重新填写",
                )
            } else {
                ResourceError::unknown()
            }
        })
    }

    async fn prepare_password(&mut self, args: &Value) -> Result<Value> {
        let origin = args["origin"].as_str().unwrap_or_default();
        let selector = args["selector"].as_str().unwrap_or_default();
        let username = args["username"].as_str().unwrap_or_default();
        let url = url::Url::parse(origin)
            .map_err(|_| ResourceError::before("invalid_origin", "站点无效"))?;
        if !matches!(url.scheme(), "http" | "https")
            || url.origin().ascii_serialization() != origin
            || selector.is_empty()
            || selector.len() > 1024
            || username.len() > 256
        {
            return Err(ResourceError::before(
                "invalid_target",
                "站点或密码选择器无效",
            ));
        }
        let id = Uuid::new_v4().to_string();
        let data = json!({"origin":origin,"selector":selector,"key":format!("__remoteops_password_{id}"),"marker":id});
        let script = format!(
            r"(() => {{ const a={data}; const nodes=document.querySelectorAll(a.selector); if(location.origin!==a.origin || nodes.length!==1) return null; const e=nodes[0]; if(!(e instanceof HTMLInputElement)||e.type!=='password'||e.disabled||e.readOnly||!e.isConnected||e.getClientRects().length===0) return null; Object.defineProperty(window,a.key,{{value:{{element:e,document,href:location.href}},configurable:true}}); window.addEventListener('pagehide',()=>{{delete window[a.key];}},{{once:true}}); document.addEventListener('visibilitychange',()=>{{if(document.hidden) delete window[a.key];}},{{once:true}}); return a.marker; }})()"
        );
        self.password_eval(&script, &id).await?;
        self.password_binding = Some(PasswordBinding {
            id: id.clone(),
            origin: origin.into(),
            selector: selector.into(),
            expires: std::time::Instant::now() + Duration::from_mins(5),
        });
        Ok(
            json!({"binding_id":id,"origin":origin,"selector":selector,"username":username,"expires_in_seconds":300}),
        )
    }

    async fn fill_password(&mut self, id: &str, password: Zeroizing<String>) -> Result<Value> {
        let binding = self
            .password_binding
            .take()
            .ok_or_else(|| ResourceError::before("binding_missing", "目标绑定已消费或不存在"))?;
        if binding.id != id
            || binding.expires <= std::time::Instant::now()
            || password.is_empty()
            || password.len() > 4096
            || self.password_redactions.len() >= 16
        {
            return Err(ResourceError::before(
                "binding_expired",
                "目标绑定不匹配或过期",
            ));
        }
        // 保留有限的内存过滤材料，不作为可复用凭据；随实例销毁清零。
        self.password_redactions.push(password.clone());
        let marker = Uuid::new_v4().to_string();
        let data = Zeroizing::new(json!({"origin":binding.origin,"selector":binding.selector,"key":format!("__remoteops_password_{id}"),"password":password.as_str(),"marker":marker}).to_string());
        let script = Zeroizing::new(format!(
            r"(() => {{ const a={}; const b=window[a.key]; delete window[a.key]; if(!b||b.document!==document||location.origin!==a.origin||location.href!==b.href) return null; const e=b.element; const nodes=document.querySelectorAll(a.selector); if(nodes.length!==1||nodes[0]!==e||!e.isConnected||e.type!=='password'||e.disabled||e.readOnly||e.getClientRects().length===0) return null; const setter=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set; setter.call(e,a.password); e.dispatchEvent(new Event('input',{{bubbles:true}})); e.dispatchEvent(new Event('change',{{bubbles:true}})); return a.marker; }})()",
            data.as_str()
        ));
        self.password_eval(&script, &marker).await?;
        Ok(json!({"status":"filled"}))
    }

    async fn password_eval(&mut self, script: &str, marker: &str) -> Result<()> {
        let result = match self.tool.startup.interface {
            ExecutionInterface::AgentBrowserCli => {
                let mut command = self.cli_command()?;
                command.args(["eval", "--stdin"]).stderr(Stdio::null());
                #[cfg(windows)]
                let mut process = spawn_in_job(
                    command,
                    self.job.clone().ok_or_else(ResourceError::unknown)?,
                )?;
                #[cfg(not(windows))]
                let mut process = spawn(command)?;
                let mut input = process
                    .child
                    .stdin()
                    .take()
                    .ok_or_else(ResourceError::unknown)?;
                let output = process
                    .child
                    .stdout()
                    .take()
                    .ok_or_else(ResourceError::unknown)?;
                let writer = async move {
                    input.write_all(script.as_bytes()).await?;
                    drop(input);
                    Ok::<(), std::io::Error>(())
                };
                let reader = async {
                    let mut bytes = Vec::new();
                    output
                        .take((MAX_OUTPUT + 1) as u64)
                        .read_to_end(&mut bytes)
                        .await?;
                    Ok::<_, std::io::Error>(Zeroizing::new(bytes))
                };
                let ((), bytes) =
                    tokio::try_join!(writer, reader).map_err(|_| ResourceError::unknown())?;
                let status = process
                    .child
                    .inner_mut()
                    .wait()
                    .await
                    .map_err(|_| ResourceError::unknown())?;
                if !status.success() || bytes.len() > MAX_OUTPUT {
                    return Err(ResourceError::unknown());
                }
                serde_json::from_slice::<Value>(&bytes).map_err(|_| ResourceError::unknown())?
            }
            ExecutionInterface::StdioMcp => {
                let (name, args) = match self.browser_adapter() {
                    BrowserAdapter::AgentBrowser => {
                        ("agent_browser_eval", json!({"script":script}))
                    }
                    BrowserAdapter::Playwright => (
                        "browser_evaluate",
                        json!({"function":format!("() => ({script})")}),
                    ),
                    BrowserAdapter::None => {
                        return Err(ResourceError::before(
                            "adapter_unsupported",
                            "工具不是受支持的浏览器",
                        ));
                    }
                };
                let schema = self.tools.iter().find(|t| t.name == name).ok_or_else(|| {
                    ResourceError::before("adapter_unsupported", "缺少秘密填写所需接口")
                })?;
                if !jsonschema::validator_for(&schema.input_schema)
                    .map_err(|_| ResourceError::unknown())?
                    .is_valid(&args)
                {
                    return Err(ResourceError::before("adapter_unsupported", "接口不匹配"));
                }
                self.rpc("tools/call", json!({"name":name,"arguments":args}), true)
                    .await?
            }
            ExecutionInterface::Cli => {
                return Err(ResourceError::before(
                    "adapter_unsupported",
                    "通用CLI不接受网页密码",
                ));
            }
        };
        if result.get("isError").and_then(Value::as_bool) == Some(true)
            || result.get("success").and_then(Value::as_bool) == Some(false)
        {
            return Err(ResourceError::unknown());
        }
        if !marker_returned(&result, marker) {
            return Err(ResourceError::before("target_changed", "目标变化"));
        }
        Ok(())
    }

    pub(super) fn redact_password_output(&self, value: &mut Value) {
        match value {
            Value::String(text) => {
                for secret in &self.password_redactions {
                    let encoded = serde_json::to_string(secret.as_str()).unwrap_or_default();
                    let url_encoded: String =
                        url::form_urlencoded::byte_serialize(secret.as_bytes()).collect();
                    let mut unicode = String::new();
                    for unit in secret.encode_utf16() {
                        use std::fmt::Write as _;
                        let _ = write!(unicode, "\\u{unit:04x}");
                    }
                    let html = secret
                        .replace('&', "&amp;")
                        .replace('<', "&lt;")
                        .replace('>', "&gt;")
                        .replace('"', "&quot;")
                        .replace('\'', "&#39;");
                    for pattern in [
                        secret.as_str(),
                        encoded.trim_matches('"'),
                        &url_encoded,
                        &html,
                        &unicode,
                        &unicode.to_uppercase(),
                    ] {
                        if !pattern.is_empty() {
                            *text = text.replace(pattern, "[秘密已隐藏]");
                        }
                    }
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.redact_password_output(item);
                }
            }
            Value::Object(items) => {
                let original = std::mem::take(items);
                for (key, mut item) in original {
                    let mut key = Value::String(key);
                    self.redact_password_output(&mut key);
                    self.redact_password_output(&mut item);
                    items.insert(key.as_str().unwrap_or("[hidden]").to_owned(), item);
                }
            }
            _ => {}
        }
    }
}

fn marker_returned(value: &Value, marker: &str) -> bool {
    match value {
        Value::String(text) => {
            text == marker
                || text
                    .lines()
                    .any(|line| line.trim() == serde_json::to_string(marker).unwrap_or_default())
        }
        Value::Array(items) => items.iter().any(|item| marker_returned(item, marker)),
        Value::Object(items) => items
            .iter()
            .filter(|(key, _)| !matches!(key.as_str(), "code" | "script" | "command" | "arguments"))
            .any(|(_, value)| marker_returned(value, marker)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn echoed_script_is_not_a_success_receipt() {
        let marker = "single-use-success-marker";
        assert!(!marker_returned(
            &json!({"content":[{"text":format!("### Ran code\nreturn '{marker}'\n### Result\nnull")}]}),
            marker
        ));
        assert!(marker_returned(
            &json!({"content":[{"text":format!("### Result\n\"{marker}\"\n### Ran code\nignored")}]}),
            marker
        ));
        assert!(marker_returned(&json!({"data":{"result":marker}}), marker));
    }
}
