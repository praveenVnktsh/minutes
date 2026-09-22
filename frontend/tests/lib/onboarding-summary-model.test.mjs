import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import vm from 'node:vm';
import ts from 'typescript';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

const modulePath = path.join(
  path.dirname(fileURLToPath(import.meta.url)),
  '..',
  '..',
  'src',
  'lib',
  'onboarding-summary-model.ts'
);
const require = createRequire(import.meta.url);
const srcRoot = path.join(path.dirname(fileURLToPath(import.meta.url)), '..', '..', 'src');

// The module under test imports '@/lib/download-display', the single formatter
// every download size in the UI goes through. tsconfig resolves that alias;
// a bare require inside the vm context does not, so map it here.
function requireWithAlias(specifier) {
  if (specifier.startsWith('@/')) {
    return loadTsModule(path.join(srcRoot, `${specifier.slice(2)}.ts`));
  }
  return require(specifier);
}

function loadTsModule(filePath) {
  const source = fs.readFileSync(filePath, 'utf8');
  const compiled = ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2020,
    },
  }).outputText;

  const module = { exports: {} };
  vm.runInNewContext(compiled, {
    exports: module.exports,
    module,
    require: requireWithAlias,
  });
  return module.exports;
}

const {
  formatSummaryModelSizeLabelFromMb,
  resolveOnboardingSummaryModelStatus,
} = loadTsModule(modulePath);

assert.equal(
  JSON.stringify(resolveOnboardingSummaryModelStatus({
    selectedModel: 'qwen3.5:4b',
    recommendedModel: 'qwen3.5:4b',
    selectedModelReady: false,
  })),
  JSON.stringify({
    selectedSummaryModel: 'qwen3.5:4b',
    summaryModelDownloaded: false,
  }),
  'legacy Gemma availability must not make an undownloaded selected Qwen model ready'
);

assert.equal(
  JSON.stringify(resolveOnboardingSummaryModelStatus({
    selectedModel: 'gemma3:1b',
    recommendedModel: 'qwen3.5:4b',
    selectedModelReady: true,
  })),
  JSON.stringify({
    selectedSummaryModel: 'gemma3:1b',
    summaryModelDownloaded: true,
  }),
  'explicit selected model should win over a different recommendation'
);

assert.equal(
  JSON.stringify(resolveOnboardingSummaryModelStatus({
    selectedModel: '',
    recommendedModel: 'qwen3.5:2b',
    selectedModelReady: true,
  })),
  JSON.stringify({
    selectedSummaryModel: 'qwen3.5:2b',
    summaryModelDownloaded: true,
  }),
  'recommended Qwen should become the selected model when no model is selected yet'
);

// The MB size table these lines used to assert is gone: sizes now reach the UI
// as `size_bytes` from the Rust catalogue, so there is nothing here to drift.
// What survives is the label helper the settings model list still calls, which
// must format in the same binary base as every other size on screen.
assert.equal(formatSummaryModelSizeLabelFromMb(2614), '~2.55 GiB');
assert.equal(formatSummaryModelSizeLabelFromMb(1221), '~1.19 GiB');
assert.equal(formatSummaryModelSizeLabelFromMb(512), '~512.0 MiB');
assert.equal(formatSummaryModelSizeLabelFromMb(0), '');
