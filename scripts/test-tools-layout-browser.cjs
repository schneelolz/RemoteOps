const assert = require("node:assert/strict");
const path = require("node:path");

/** 使用真实隔离 Relay 的 API 结果增加展示夹具，不向真实节点发送操作。 */
module.exports = async function verifyToolsLayout(page, origin, artifacts) {
  let checks = 0;
  const go = async (route) => {
    await page.goto(origin + "/#/" + route);
    await page.waitForTimeout(150);
  };
  await go("tools");
  await page.getByRole("heading", { name: "全局设置", exact: true }).waitFor();
  assert(page.url().endsWith("/tools/settings"));
  checks++;
  await go("tools/list");
  await page.getByRole("link", { name: "工具中心", exact: true }).click();
  await page.getByRole("heading", { name: "全局设置", exact: true }).waitFor();
  checks++;
  await page.goto(origin + "/#tab=tools");
  await page.reload();
  await page.getByRole("heading", { name: "全局设置", exact: true }).waitFor();
  checks++;
  await page.goto(origin + "/#tab=tools&section=list");
  await page.reload();
  await page.getByRole("heading", { name: "工具列表", exact: true }).waitFor();
  checks++;
  await go("tools/sync");
  await go("tools/list");
  await page.goBack();
  await page
    .getByRole("heading", { name: "节点同步状态", exact: true })
    .waitFor();
  await page.goForward();
  await page.getByRole("heading", { name: "工具列表", exact: true }).waitFor();
  await page.reload();
  await page.getByRole("heading", { name: "工具列表", exact: true }).waitFor();
  checks += 3;
  const base = (
    await (await page.request.get(origin + "/api/admin/tools")).json()
  ).data.tools[0];
  assert(base);
  const handler = async (route) => {
    if (route.request().method() !== "GET") return route.fallback();
    const response = await route.fetch();
    const payload = await response.json();
    payload.data.nodes = Array.from({ length: 266 }, (_, i) => {
      const status = [
        "synced",
        "pending",
        "update_pending",
        "apply_failed",
        "offline",
        "upgrade_required",
      ][i % 6];
      return {
        agent_instance_id: `layout-node-${i}`,
        hostname: `布局节点-${String(i).padStart(3, "0")}`,
        supported: status !== "upgrade_required",
        online: status !== "offline",
        applied_revision: status === "pending" ? null : payload.data.revision,
        synced: status === "synced",
        sync_status: status,
      };
    });
    payload.data.tools = [
      base,
      ...Array.from({ length: 64 }, (_, i) => {
        const tool = structuredClone(base);
        tool.id = `layout-${String(i).padStart(3, "0")}`;
        tool.name = `布局工具 ${String(i).padStart(3, "0")}`;
        if (i === 63) {
          tool.id = "long-id-" + "abcdef0123456789".repeat(7);
          tool.name = "长名称工具用于检查窄列换行及详细信息展示".repeat(3);
        }
        tool.purpose = "长用途说明，验证两行截断、内容居中和列宽分配。".repeat(
          8,
        );
        tool.instructions = "测试使用说明。\n".repeat(40);
        tool.examples = ["示例操作，不执行远程操作。".repeat(20)];
        tool.dependencies = ["依赖说明"];
        tool.versions.push({
          ...structuredClone(tool.versions[0]),
          version: "2.0.0",
          packages: tool.versions[0].packages.map((p) => ({
            ...structuredClone(p),
            tests: [],
          })),
        });
        return tool;
      }),
    ];
    await route.fulfill({ response, json: payload });
  };
  await page.route("**/api/admin/tools", handler);
  try {
    await go("tools/settings");
    await go("tools/list");
    await page.waitForFunction(
      () => document.querySelectorAll(".tools-table tbody tr").length === 20,
    );
    assert.equal(await page.locator(".tools-table tbody tr").count(), 20);
    checks++;
    await page
      .locator(".tools-list-pagination")
      .getByRole("button", { name: "下一页", exact: true })
      .click();
    assert(
      (await page.locator(".tools-list-pagination").textContent()).includes(
        "第 2 / 4 页",
      ),
    );
    checks++;
    let before = 0;
    const trigger = page
      .locator(".tools-table tbody tr")
      .first()
      .getByRole("button", { name: "详情", exact: true });
    await trigger.click();
    before = await page.locator(".content-area").evaluate((el) => el.scrollTop);
    await page.getByRole("tab", { name: "使用指南", exact: true }).waitFor();
    await page.getByRole("tab", { name: "使用指南", exact: true }).focus();
    await page.keyboard.press("ArrowRight");
    assert.equal(
      await page
        .getByRole("tab", { name: "版本与平台包", exact: true })
        .getAttribute("aria-selected"),
      "true",
    );
    await page.keyboard.press("Home");
    assert.equal(
      await page
        .getByRole("tab", { name: "使用指南", exact: true })
        .getAttribute("aria-selected"),
      "true",
    );
    checks++;
    assert.equal(await page.getByRole("dialog").count(), 1);
    assert.equal(await page.locator(".tools-details").count(), 0);
    checks++;
    await page.getByRole("tab", { name: "版本与平台包", exact: true }).click();
    await page.getByLabel("工具版本", { exact: true }).selectOption("2.0.0");
    await page.getByRole("tab", { name: "测试记录", exact: true }).click();
    await page
      .getByText("尚未验证。文件校验通过不代表当前平台可正常运行。", {
        exact: true,
      })
      .waitFor();
    checks++;
    await page
      .getByRole("button", { name: "登记实际环境测试", exact: true })
      .click();
    assert.equal(await page.getByRole("dialog").count(), 1);
    await page.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal(
      await page
        .getByRole("tab", { name: "测试记录", exact: true })
        .getAttribute("aria-selected"),
      "true",
    );
    assert.equal(
      await page.getByLabel("工具版本", { exact: true }).inputValue(),
      "2.0.0",
    );
    checks++;
    await page
      .getByRole("button", { name: "编辑工具资料", exact: true })
      .click();
    await page.getByRole("button", { name: "取消", exact: true }).click();
    assert.equal(await page.getByRole("dialog").count(), 1);
    await page.keyboard.press("Escape");
    await page.waitForTimeout(50);
    assert.equal(
      await trigger.evaluate((el) => document.activeElement === el),
      true,
    );
    assert(
      (await page.locator(".tools-list-pagination").textContent()).includes(
        "第 2 / 4 页",
      ),
    );
    assert.equal(
      await page.locator(".content-area").evaluate((el) => el.scrollTop),
      before,
    );
    checks += 3;
    await page.getByLabel("搜索工具", { exact: true }).fill("不存在的工具");
    await page
      .getByRole("heading", { name: "暂无匹配工具", exact: true })
      .waitFor();
    checks++;
    await page.getByLabel("搜索工具", { exact: true }).fill("long-id-");
    assert.equal(await page.locator(".tools-table tbody tr").count(), 1);
    assert(
      (await page.locator(".tools-list-pagination").textContent()).includes(
        "第 1 / 1 页",
      ),
    );
    await page.getByLabel("搜索工具", { exact: true }).fill("");
    await page.getByLabel("每页工具数", { exact: true }).selectOption("50");
    assert.equal(await page.locator(".tools-table tbody tr").count(), 50);
    await page.getByLabel("每页工具数", { exact: true }).selectOption("100");
    assert.equal(await page.locator(".tools-table tbody tr").count(), 65);
    await page.getByLabel("每页工具数", { exact: true }).selectOption("20");
    checks += 3;
    for (const [device, viewport] of [
      ["desktop", { width: 1440, height: 900 }],
      ["large", { width: 2552, height: 1274 }],
      ["short", { width: 1366, height: 768 }],
      ["mobile", { width: 390, height: 844 }],
    ]) {
      await page.setViewportSize(viewport);
      for (const theme of ["light", "dark"]) {
        await page.getByLabel("界面主题").selectOption(theme);
        await go("tools/sync");
        await page.locator(".tools-sync-table tbody tr").first().waitFor();
        await page.screenshot({
          path: path.join(artifacts, `${device}-${theme}-layout-sync.png`),
          fullPage: true,
        });
        assert.equal(
          await page.evaluate(
            () => document.documentElement.scrollWidth > innerWidth,
          ),
          false,
        );
        if (viewport.width >= 768 && viewport.height >= 800) {
          const body = await page.locator(".content-area").boundingBox();
          const table = await page
            .locator(".tools-sync-container")
            .boundingBox();
          assert(
            body &&
              table &&
              Math.abs(body.y + body.height - 30 - (table.y + table.height)) <=
                3,
            "table fills remaining height",
          );
          checks++;
        }
        await page.locator(".tools-sync-scroll").evaluate((el) => {
          el.scrollTop = 120;
        });
        if (viewport.width >= 768) {
          assert(
            await page
              .locator(".tools-sync-table th")
              .first()
              .evaluate((el) => {
                const r = el.getBoundingClientRect();
                return el.contains(
                  document.elementFromPoint(r.x + 10, r.y + 10),
                );
              }),
            "sticky header visible over rows",
          );
          checks++;
        }
        await page
          .locator(".tools-pagination")
          .getByRole("button", { name: "下一页", exact: true })
          .click();
        assert.equal(
          await page
            .locator(".tools-sync-scroll")
            .evaluate((el) => el.scrollTop),
          0,
        );
        checks++;
        await go("tools/list");
        await page
          .getByRole("button", { name: "详情", exact: true })
          .nth(1)
          .click();
        for (const tab of ["使用指南", "版本与平台包", "测试记录"]) {
          await page.getByRole("tab", { name: tab, exact: true }).click();
          await page.screenshot({
            path: path.join(artifacts, `${device}-${theme}-detail-${tab}.png`),
            fullPage: true,
          });
          const footer = await page.locator(".modal-footer").boundingBox();
          assert(footer && footer.y + footer.height <= viewport.height + 1);
          assert.equal(await page.getByRole("dialog").count(), 1);
          checks++;
        }
        await page.keyboard.press("Escape");
        await page.screenshot({
          path: path.join(artifacts, `${device}-${theme}-layout-list.png`),
          fullPage: true,
        });
        if (viewport.width >= 768) {
          assert.equal(
            await page
              .locator(".tools-table tbody td")
              .first()
              .evaluate((el) => getComputedStyle(el).verticalAlign),
            "middle",
          );
          checks++;
        }
      }
    }
  } finally {
    await page.unroute("**/api/admin/tools", handler);
    await page.setViewportSize({ width: 1440, height: 900 });
    await go("tools/list");
  }
  return checks;
};
