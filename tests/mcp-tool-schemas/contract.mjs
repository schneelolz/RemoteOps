import assert from 'node:assert/strict';
import Ajv2020 from 'ajv/dist/2020.js';
import { ListToolsResultSchema } from '@modelcontextprotocol/sdk/types.js';

export const sdkVersion = '1.32.2';
const ajv = new Ajv2020({ strict: true, validateFormats: false, allowUnionTypes: true });

/** 按真实分页顺序检查完整目录，不能只验证第一页或首个工具。 */
export async function validateToolPages(getPage) {
  const tools = [];
  const names = new Set();
  const cursors = new Set();
  let cursor;
  let pages = 0;
  do {
    const raw = await getPage(cursor);
    // 必须先检查 MCP 契约；仅 JSON Schema 元 Schema 校验会放过缺少根类型的工具。
    ListToolsResultSchema.parse(raw);
    for (const [index, tool] of raw.tools.entries()) {
      const location = `page[${pages}].tools[${index}](${tool.name})`;
      assert.match(tool.name, /^[A-Za-z0-9_.-]{1,128}$/, `${location}.name`);
      assert(!names.has(tool.name), `${location}.name duplicate`);
      names.add(tool.name);
      assert(tool.annotations && typeof tool.annotations === 'object', `${location}.annotations`);
      for (const annotation of ['readOnlyHint', 'destructiveHint', 'idempotentHint', 'openWorldHint']) {
        assert.equal(typeof tool.annotations[annotation], 'boolean', `${location}.annotations.${annotation}`);
      }
      for (const keyword of ['inputSchema', 'outputSchema']) {
        if (keyword === 'outputSchema' && tool[keyword] === undefined) continue;
        const schema = tool[keyword];
        assert.equal(schema.type, 'object', `${location}.${keyword}.type`);
        assert(ajv.validateSchema(schema), `${location}.${keyword}: ${ajv.errorsText()}`);
        ajv.compile(schema);
      }
      tools.push(tool);
    }
    pages++;
    cursor = raw.nextCursor;
    if (cursor !== undefined) {
      assert(!cursors.has(cursor), 'Repeated tools/list cursor');
      cursors.add(cursor);
      assert(pages < 1000, 'Too many tools/list pages');
    }
  } while (cursor !== undefined);
  assert(tools.length > 0, 'Empty tool list');
  return { tools, pages };
}

/** 校验真实返回对象、structuredContent 与兼容文本 JSON 保持完全一致。 */
export function validateToolResult(tool, result) {
  assert.notEqual(result.isError, true, `${tool.name}: unexpected MCP execution error`);
  assert(result.structuredContent && typeof result.structuredContent === 'object', `${tool.name}.structuredContent`);
  const validate = ajv.compile(tool.outputSchema);
  assert(validate(result.structuredContent), `${tool.name}: ${ajv.errorsText(validate.errors)}`);
  const text = result.content.filter(content => content.type === 'text');
  assert.equal(text.length, 1, `${tool.name}: expected one compatible JSON text block`);
  assert.deepEqual(JSON.parse(text[0].text), result.structuredContent);
  if (tool.name === 'manage_relay_tools' || tool.name === 'upload_relay_tool_package') assert.deepEqual(Object.keys(result.structuredContent).sort(), ['data', 'error', 'request_id', 'revision']);
  return result.structuredContent;
}
