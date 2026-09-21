import { createInterface } from 'node:readline';

for await (const line of createInterface({ input: process.stdin })) {
  const request = JSON.parse(line);
  if (request.op === 'init') {
    process.stdout.write(`${JSON.stringify({ v: 1, id: request.id, ok: true, ready: true })}\n`);
  } else if (request.op === 'collect') {
    const sample = {
      channel: request.channel, origin: 'https://www.twitch.tv', status: 'ok', reason: 'sample_complete',
      usernames: ['Alice_1'], role_lists: 1, scroll_rounds: 1, reached_end: true,
      ready_state: 'complete', document_lang: 'en-US', known_error_title_present: false,
      viewer_toggle_present: true, viewer_input_present: true, rendered_row_count: 1,
      login_prompt_present: false,
    };
    process.stdout.write(`${JSON.stringify({ v: 1, id: request.id, ok: true, sample })}\n`);
  } else if (request.op === 'shutdown') {
    process.stdout.write(`${JSON.stringify({ v: 1, id: request.id, ok: true, ready: true })}\n`);
    break;
  }
}
