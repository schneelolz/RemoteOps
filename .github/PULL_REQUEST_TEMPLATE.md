## 变更说明

请说明问题、实现边界和用户可见影响。

## 验证

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo check --workspace --locked`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --locked`
- [ ] 文档与敏感信息已复核

## 安全检查

- [ ] 未提交 Token、密码、证书、私钥、控制码、客户信息或本地状态文件
- [ ] 新增远程操作已明确风险级别、目标绑定和审批要求

