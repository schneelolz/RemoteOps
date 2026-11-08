import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import Ajv2020 from 'ajv/dist/2020.js';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { validateToolPages, validateToolResult } from './contract.mjs';

const repository = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const executable = process.env.REMOTEOPS_SCHEMA_MCP_EXECUTABLE
  || path.join(repository, 'target/debug', `remoteops-controller-mcp${process.platform === 'win32' ? '.exe' : ''}`);
const reports = new Map();
function actualTools(testUi = false) {
  if (!reports.has(testUi)) {
    const args = ['--check-tool-schemas', '--config', path.join(repository, 'artifacts/does-not-exist/invalid-config.json')];
    if (testUi) args.push('--enable-test-ui');
    const child = spawnSync(executable, args, { windowsHide: true, encoding: 'utf8', timeout: 30000 });
    assert.equal(child.status, 0, child.stderr || child.error?.message);
    const report = JSON.parse(child.stdout);
    assert.equal(report.passed, true);
    assert.equal(report.tool_count, report.tools.length);
    assert.deepEqual(report.failures, []);
    reports.set(testUi, report.tools);
  }
  return structuredClone(reports.get(testUi));
}

function paged(tools, pageSize = 17) {
  return async cursor => {
    const offset = cursor === undefined ? 0 : Number(cursor);
    return { tools: tools.slice(offset, offset + pageSize),
      ...(offset + pageSize < tools.length ? { nextCursor: String(offset + pageSize) } : {}) };
  };
}

for (const enabled of [false, true]) {
  test(`actual ${enabled ? 'test UI' : 'default'} router, complete pagination and annotations`, async () => {
    const tools = actualTools(enabled);
    const result = await validateToolPages(paged(tools));
    assert(result.pages > 1);
    assert.equal(result.tools.length, tools.length);
    for (const name of ['test_prompt_text', 'test_prompt_password']) {
      assert.equal(tools.some(tool => tool.name === name), enabled);
    }
    for (const name of ['manage_relay_tools', 'upload_relay_tool_package']) {
      const tool = tools.find(tool => tool.name === name);
      assert.equal(tool.outputSchema.type, 'object');
      assert.deepEqual(tool.outputSchema.properties.data, {});
      for (const data of [null, false, 1, 'arbitrary', [], { enabled: false }, { file_verified: true }]) {
        for (const error of [null, { code: 'revision_conflict', message: 'fixture business error' }]) {
          const expected = { request_id: 'fixture-request', revision: 1, error, data };
          assert.deepEqual(validateToolResult(tool, {
            content: [{ type: 'text', text: JSON.stringify(expected) }], structuredContent: expected,
          }), expected);
        }
      }
    }
  });
}

test('negative: omitted properties remains valid when root object type is present', async () => {
  const tool = actualTools()[0];
  tool.inputSchema = { type: 'object' };
  tool.outputSchema = { type: 'object' };
  await validateToolPages(paged([tool]));
});

for (const [name, mutate] of [
  ['missing root object type', tool => { tool.outputSchema = { $schema: 'https://json-schema.org/draft/2020-12/schema' }; }],
  ['invalid nested type', tool => { tool.outputSchema = { type: 'object', properties: { nested: { type: 'not-a-type' } } }; }],
  ['invalid annotation', tool => { tool.annotations.readOnlyHint = 'yes'; }],
  ['boolean property schema', tool => { tool.outputSchema = { type: 'object', properties: { data: true } }; }],
]) {
  for (const pageSize of [1000, 17]) {
    test(`negative: ${name} at last tool, page size ${pageSize}`, async () => {
      const tools = actualTools();
      mutate(tools.at(-1));
      await assert.rejects(validateToolPages(paged(tools, pageSize)));
    });
  }
}

test('negative: duplicate name on the second page', async () => {
  const tools = actualTools();
  tools[17].name = tools[0].name;
  await assert.rejects(validateToolPages(paged(tools)), /duplicate/);
});

test('negative: missing root type on the second page', async () => {
  const tools = actualTools();
  tools[17].outputSchema = { properties: {} };
  await assert.rejects(validateToolPages(paged(tools)));
});

test('negative: result schema mismatch or incompatible text JSON', () => {
  const tool = actualTools().find(tool => tool.name === 'manage_relay_tools');
  const output = { request_id: 'fixture-request', revision: 'invalid', error: null, data: null };
  assert.throws(() => validateToolResult(tool, {
    content: [{ type: 'text', text: JSON.stringify(output) }], structuredContent: output,
  }));
  output.revision = 0;
  assert.throws(() => validateToolResult(tool, {
    content: [{ type: 'text', text: '{}' }], structuredContent: output,
  }));
});


test('actual manage input discovers full public contract and validates standalone Skill examples', () => {
  const tool = actualTools().find(tool => tool.name === 'manage_relay_tools');
  const ajv = new Ajv2020({ strict: true, validateFormats: false, allowUnionTypes: true });
  const validate = ajv.compile(tool.inputSchema);
  const definitions = tool.inputSchema.$defs;
  const command = definitions.ToolCenterCommand;
  const actions = command.oneOf.map(branch => branch.properties.action.const);
  assert.deepEqual(actions, ['snapshot', 'set_policy', 'set_guide', 'put_tool', 'set_tool_enabled', 'record_test', 'import_external']);
  assert(validate({ request_id: 'guide-fixture', expected_revision: 0, command: { action: 'set_guide', guide_markdown: '# guide' } }));
  assert.deepEqual(definitions.BrowserAdapter.enum || definitions.BrowserAdapter.oneOf.map(branch => branch.const), ['none', 'playwright', 'agent_browser']);
  assert.deepEqual(definitions.ExecutionInterface.enum || definitions.ExecutionInterface.oneOf.map(branch => branch.const), ['stdio_mcp', 'agent_browser_cli', 'cli']);
  assert.deepEqual(definitions.ResourcePackage.properties.format.enum, ['zip']);
  assert.equal(definitions.ManagedTool.properties.enabled.default, false);
  assert.equal(definitions.ToolPackage.properties.file_verified.default, false);
  assert.deepEqual(definitions.ToolPackage.properties.tests.default, []);
  for (const model of ['ManagedTool', 'ToolVersion', 'ToolPackage', 'PackageSource', 'PlatformTest', 'ResourceTool',
    'ToolDocumentation', 'Platform', 'ResourcePackage', 'Startup', 'HealthCheck', 'CliOperation', 'CliArgument', 'CliResultFormat']) {
    assert(definitions[model], model);
  }
  const examples = {};
  for (const name of ['put-tool-external', 'put-tool-hosted', 'record-test']) {
    const input = JSON.parse(readFileSync(path.join(repository, 'deploy/skills/remoteops/references/examples', `${name}.json`), 'utf8'));
    assert(validate(input), `${name}: ${ajv.errorsText(validate.errors)}`);
    examples[name] = input;
  }
  for (const action of ['begin_upload', 'upload_chunk', 'finish_upload', 'cancel_upload']) {
    assert.equal(validate({ request_id: 'isolated', expected_revision: 0, command: { action } }), false);
  }
  for (const mutate of [
    input => { delete input.command.tool.name; },
    input => { delete input.command.tool.versions[0].version; },
    input => { delete input.command.tool.versions[0].packages[0].tool.platforms[0].arch; },
    input => { input.command.tool.versions[0].packages[0].tool.startup.interface = 'zip'; },
    input => { input.command.tool.versions[0].packages[0].tool.package.format = 'stdio_mcp'; },
    input => { input.command.tool.versions[0].packages[0].source.kind = 'unknown'; },
    input => { input.command.tool.versions[0].packages[0].tool.startup.args = [false]; },
  ]) {
    const input = structuredClone(examples['put-tool-external']);
    mutate(input);
    assert.equal(validate(input), false);
  }
  const input = structuredClone(examples['record-test']);
  input.command.record = {};
  assert.equal(validate(input), false);
});
