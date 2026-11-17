// Not run: `npm run check-types` compiles it against index.d.ts through the package's exports,
// as a TypeScript project that depends on rustyprint would, and fails if the types disagree.

import { readFileSync } from 'node:fs'
import {
  contentWidth,
  contentWidthSync,
  createRecordSet,
  createRenderer,
  InvalidOptionsError,
  nativeVersion,
  render,
  renderSync,
  RecordSetError,
  RustyprintError,
  toReadable,
  VERSION,
  type RecordSet,
  type RenderOptions,
  type RenderOutput
} from 'rustyprint'

const options: RenderOptions = {
  pageSize: { width: '100mm', height: '150mm' },
  landscape: false,
  margin: '15mm 20mm',
  title: 'Invoice 42',
  stylesheet: 'h1 { color: teal }',
  remote: { allowedHosts: ['cdn.example.com'], allowHttp: false, timeoutMs: 5000 },
  limits: { maxPages: 20, timeoutMs: null },
  assets: { 'logo.png': readFileSync('logo.png'), 'page.css': '@page { margin: 1cm }' }
}

async function main (): Promise<void> {
  const output: RenderOutput = await render('<h1>Hello</h1>', { ...options, signal: AbortSignal.timeout(5000) })
  const pdf: Buffer = output.pdf
  const warnings: string[] = output.warnings

  renderSync(new TextEncoder().encode('<p>x</p>'), { pageSize: 'Letter', assets: new Map([['a.css', 'p {}']]) })

  const renderer = createRenderer({ concurrency: 2 })
  await renderer.render('<p>x</p>')
  await renderer.close()

  const widths: number[] = [
    await contentWidth('<p>x</p>', { width: 1280, height: 720 }, { stylesheet: 'p {}', signal: AbortSignal.timeout(5000) }),
    contentWidthSync('<p>x</p>', { width: 1280, height: 720 }),
    await renderer.contentWidth('<p>x</p>', { width: 1280, height: 720 })
  ]

  const set: RecordSet = createRecordSet({ ...options, limits: { maxTotalPages: 1000 }, signal: AbortSignal.timeout(5000) })
  const added: { pdf: Buffer, pages: number, warnings: string[] } = await set.add('<p>x</p>')
  const continued: { pdf: Buffer, pages: number, warnings: string[] } = await set.add('<p>y</p>', { continue: true })
  const summary: { pdf: Buffer, pages: number, records: number } = await set.finish()
  await set.abort()
  const rows = ['<p>1</p>', '<p>2</p>']
  const stream: NodeJS.ReadableStream = toReadable(renderer.createRecordSet(), async () => rows.shift() ?? null, { highWaterMark: 0 })
  const slices = [{ html: '<table></table>' }, { html: '<table data-rp-continue></table>', continue: true }]
  const sliced: NodeJS.ReadableStream = toReadable(createRecordSet(), () => slices.shift() ?? null)
  const recordSetKind: 'record_set' = new RecordSetError('x').kind

  const versions: string[] = [VERSION, nativeVersion()]

  try {
    renderSync('<p>x</p>', { pageSize: 'B99' })
  } catch (error) {
    if (error instanceof InvalidOptionsError) {
      const kind: 'invalid_options' = error.kind
    } else if (error instanceof RustyprintError) {
      const kind: 'invalid_options' | 'limit_exceeded' | 'timeout' | 'record_set' | 'internal' = error.kind
    }
  }

  // @ts-expect-error: a signal stops only renders waiting for a worker, so renderSync takes none.
  renderSync('<p>x</p>', { signal: AbortSignal.abort() })

  void [pdf, warnings, versions, widths, added, continued, summary, stream, sliced, recordSetKind]
}

void main
