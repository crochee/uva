#!/usr/bin/env node
/**
 * 人类可读的契约报告生成器: docs/interface-contract/contract.md
 */
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { readFileSync, writeFileSync, existsSync } from 'node:fs';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const IN_YAML = resolve(ROOT, 'docs/interface-contract/contract.yaml');
const OUT_MD = resolve(ROOT, 'docs/interface-contract/contract.md');

interface Endpoint {
  id: string;
  method: string;
  path: string;
  source: 'backend' | 'frontend' | 'both';
  source_files: { backend?: string[]; frontend?: string[] };
  notes?: string;
  sse_events?: { name: string; fields: string[]; cadence_ms?: number }[];
  frontend_consumed?: boolean;
}
interface ContractFile {
  version: number;
  generated_at: string;
  endpoints: Endpoint[];
}

function main() {
  if (!existsSync(IN_YAML)) {
    console.error(`contract.yaml not found at ${IN_YAML}. Run \`npm run scan\` first.`);
    process.exit(2);
  }
  const cf = JSON.parse(
    readFileSync(resolve(ROOT, 'docs/interface-contract/contract.json'), 'utf8'),
  ) as ContractFile;

  const counts = {
    total: cf.endpoints.length,
    both: cf.endpoints.filter((e) => e.source === 'both').length,
    backend: cf.endpoints.filter(
      (e) => e.source === 'backend' && e.frontend_consumed !== false,
    ).length,
    frontend: cf.endpoints.filter((e) => e.source === 'frontend').length,
    // Backend routes the web app deliberately does not call: they
    // serve another consumer (an Anthropic-compatible client, an ops
    // probe, the attachment store's own API). The check does not
    // report them as drift, so the report must not count them as
    // unpaired either.
    webFree: cf.endpoints.filter(
      (e) => e.source === 'backend' && e.frontend_consumed === false,
    ).length,
  };

  const lines: string[] = [];
  lines.push('# Elysper 接口契约并集表');
  lines.push('');
  lines.push(`> 生成时间: ${cf.generated_at}`);
  lines.push(`> 版本: ${cf.version}`);
  lines.push('');
  lines.push('## 统计');
  lines.push('');
  lines.push(`- Total: ${counts.total}`);
  lines.push(`- Paired (双侧一致): ${counts.both}`);
  lines.push(`- Backend-only (后端提供, 前端未调用): ${counts.backend}`);
  lines.push(`- Web-free (后端提供, 前端按设计不调用): ${counts.webFree}`);
  lines.push(`- Frontend-only (前端调用, 后端未注册): ${counts.frontend}`);
  lines.push('');
  lines.push('## 端点');
  lines.push('');
  for (const e of cf.endpoints) {
    lines.push(`### ${e.method} \`${e.path}\``);
    lines.push('');
    lines.push(`- **状态**: ${e.source}`);
    if (e.source_files.backend?.length) {
      lines.push(`- **后端来源**: ${e.source_files.backend.map((s) => '`' + s + '`').join(', ')}`);
    }
    if (e.source_files.frontend?.length) {
      lines.push(`- **前端来源**: ${e.source_files.frontend.map((s) => '`' + s + '`').join(', ')}`);
    }
    if (e.sse_events?.length) {
      lines.push(`- **SSE 事件**:`);
      for (const ev of e.sse_events) {
        lines.push(`  - ${ev.name}: ${ev.fields.join(', ')}${ev.cadence_ms ? ` (cadence ${ev.cadence_ms}ms)` : ''}`);
      }
    }
    if (e.frontend_consumed === false) {
      lines.push('- **前端消费**: 否（web 之外的消费者，见 Note）');
    }
    if (e.notes) lines.push(`- **Note**: ${e.notes}`);
    lines.push('');
  }
  writeFileSync(OUT_MD, lines.join('\n'), 'utf8');
  console.log(`[contract-report] -> ${OUT_MD}`);
}

main();
