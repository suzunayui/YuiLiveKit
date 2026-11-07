const {test} = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs'), path = require('node:path'), vm = require('node:vm');
const {acceptParticipantComment} = require('../participantQueue');
const {sealPassword, openPassword} = require('../obsSources');
const {migrateUserData} = require('../dataMigration');

const tick = () => new Promise(resolve => setImmediate(resolve));
const html = '"INNERTUBE_API_KEY":"test" "clientVersion":"1.0" "continuation":"first"';
const liveResponse = (actions = []) => ({continuationContents: {liveChatContinuation: {actions, continuations: [{timedContinuationData: {continuation: 'next', timeoutMs: 1000}}]}}});

function collector(respond, store = {}) {
  const sleepers = [];
  const context = vm.createContext({
    require: () => ({saveComment() {}, ...store}), module: {exports: {}},
    console: {log() {}, error() {}}, Error, URL,
    fetch: async url => url.includes('/watch?') ? {ok: true, text: async () => html} : {ok: true, json: async () => respond()},
    setTimeout: (callback, delay) => sleepers.push({callback, delay}),
  });
  vm.runInContext(fs.readFileSync(path.join(__dirname, '../youtubeChat.js'), 'utf8'), context);
  return {...context.module.exports, sleepers};
}
const parse = input => {
  const target = collector(() => liveResponse()).parseChatTarget(input);
  return JSON.parse(JSON.stringify(target));
};

test('accepts video and channel URLs along with the existing inputs', () => {
  const video = {videoId: 'abc_DEF-123'};
  for (const input of [
    'https://www.youtube.com/watch?v=abc_DEF-123',
    'https://www.youtube.com/watch?list=PL1&v=abc_DEF-123&t=10s',
    'https://m.youtube.com/watch?v=abc_DEF-123',
    'youtube.com/watch?v=abc_DEF-123',
    'https://youtu.be/abc_DEF-123?si=share',
    'youtu.be/abc_DEF-123',
    'https://www.youtube.com/live/abc_DEF-123?feature=share',
    'https://www.youtube.com/shorts/abc_DEF-123',
    'https://www.youtube.com/live_chat?is_popout=1&v=abc_DEF-123',
    '  abc_DEF-123  ',
  ]) assert.deepEqual(parse(input), video, input);

  assert.deepEqual(parse('https://www.youtube.com/@yui/live'), {livePath: '/@yui'});
  assert.deepEqual(parse('https://www.youtube.com/@yui'), {livePath: '/@yui'});
  assert.deepEqual(parse('@yui'), {livePath: '/@yui'});
  assert.deepEqual(parse('https://www.youtube.com/channel/UCabc/streams'), {livePath: '/channel/UCabc'});
  assert.deepEqual(parse('https://www.youtube.com/c/yui'), {livePath: '/c/yui'});
  assert.deepEqual(parse('UCxxxxxxxxxxxxxxxxxxxxxx'), {livePath: '/channel/UCxxxxxxxxxxxxxxxxxxxxxx'});
});

test('explains URLs that cannot lead to a stream', () => {
  const chat = collector(() => liveResponse());
  for (const [input, message] of [
    ['https://example.com/watch?v=abc_DEF-123', /YouTubeのURL/],
    ['https://www.youtube.com/watch', /動画ID/],
    ['https://www.youtube.com/watch?v=short', /動画ID/],
    ['https://www.youtube.com/feed/subscriptions', /配信を探せません/],
    ['https://', /読み取れません/],
  ]) assert.throws(() => chat.parseChatTarget(input), message, input);
});

const paidItem = amount => ({addChatItemAction: {item: {liveChatPaidMessageRenderer: {
  id: amount, authorName: {simpleText: 'Viewer'}, timestampUsec: '1000000', purchaseAmountText: {simpleText: amount}, message: {runs: [{text: 'thanks'}]},
}}}});

test('only yen amounts count toward the yen-based tiers', async () => {
  const saved = [];
  let requests = 0;
  const amounts = ['¥1,000', '￥500', 'JP¥2,000', '$5.00', 'CA$5.00', 'CN¥50', '€1.000,50'];
  const chat = collector(() => ++requests === 2 ? liveResponse(amounts.map(paidItem)) : liveResponse(), {saveComment: msg => saved.push(msg)});
  const run = chat.startLiveChat('abcdefghijk');
  await tick();
  assert.deepEqual(saved.map(msg => msg.amount), [1000, 500, 2000, null, null, null, null]);
  assert.deepEqual(saved.map(msg => msg.amount_text), amounts);
  chat.stopLiveChat(); chat.sleepers.shift().callback(); await run;
});

const textItem = (id, channel) => ({addChatItemAction: {item: {liveChatTextMessageRenderer: {
  id, authorExternalChannelId: channel, authorName: {simpleText: 'Viewer'}, timestampUsec: '1000000', message: {runs: [{text: 'hi'}]},
}}}});

test('removal actions hide comments by id and by author, after the batch is saved', async () => {
  const calls = [];
  let requests = 0;
  const chat = collector(() => ++requests === 2 ? liveResponse([
    {markChatItemAsDeletedAction: {targetItemId: 'marked'}},
    textItem('new', 'channel-new'),
    {removeChatItemAction: {targetItemId: 'removed'}},
    {markChatItemsByAuthorAsDeletedAction: {externalChannelId: 'banned'}},
    {removeChatItemByAuthorAction: {externalChannelId: 'purged'}},
  ]) : liveResponse(), {
    saveComment: msg => calls.push(['save', msg.id, msg.author_channel_id]),
    markCommentDeleted: id => calls.push(['id', id]),
    markAuthorCommentsDeleted: (channel, video) => calls.push(['author', channel, video]),
  });

  const run = chat.startLiveChat('abcdefghijk');
  await tick();
  assert.deepEqual(calls.map(call => [...call]), [
    ['save', 'new', 'channel-new'], ['id', 'marked'], ['id', 'removed'],
    ['author', 'banned', 'abcdefghijk'], ['author', 'purged', 'abcdefghijk'],
  ]);
  chat.stopLiveChat(); chat.sleepers.shift().callback(); await run;
});

test('collection stops after repeated responses without a continuation', async () => {
  let requests = 0;
  const chat = collector(() => ++requests === 1 ? liveResponse() : {responseContext: {}});
  const states = [];
  const run = chat.startLiveChat('abcdefghijk', state => states.push(state));
  await tick();
  assert.deepEqual(states, ['reconnecting']);
  chat.sleepers.shift().callback(); await tick();
  chat.sleepers.shift().callback();
  await assert.rejects(run, /配信が終了/);
  assert.equal(states.length, 3);
});

test('a single empty response recovers and keeps collecting', async () => {
  let requests = 0;
  const chat = collector(() => ++requests === 2 ? {responseContext: {}} : liveResponse());
  const states = [];
  const run = chat.startLiveChat('abcdefghijk', state => states.push(state));
  await tick();
  chat.sleepers.shift().callback(); await tick();
  assert.deepEqual(states, ['reconnecting', 'running']);
  chat.stopLiveChat(); chat.sleepers.shift().callback(); await run;
});

const request = (id, author = 'Viewer') => ({id, author, kind: 'text', text: '参加希望です', video_id: 'v', timestamp_ms: 1000});
const replay = (bucket, comments) => comments.forEach(comment => acceptParticipantComment(bucket, comment));
const statuses = bucket => [...bucket.values()].map(item => [item.id, item.status]);

test('a repeat request stays out of the queue after the first is handled', () => {
  const bucket = new Map(), comments = [request('first'), request('second')];
  replay(bucket, comments);
  assert.deepEqual(statuses(bucket), [['first', 'waiting'], ['second', 'deleted']]);

  bucket.get('Viewer::first').status = 'done';
  replay(bucket, comments);
  assert.deepEqual(statuses(bucket), [['first', 'done'], ['second', 'deleted']]);
});

test('clearing the queue does not bring re-read comments back', () => {
  const bucket = new Map(), comments = [request('a', 'A'), request('b', 'A'), request('c', 'B')];
  replay(bucket, comments);
  for (const item of bucket.values()) item.status = 'deleted';
  replay(bucket, comments);
  assert.ok([...bucket.values()].every(item => item.status === 'deleted'));
});

test('someone already handled can request again with a new comment', () => {
  const bucket = new Map();
  replay(bucket, [request('first')]);
  bucket.get('Viewer::first').status = 'done';
  assert.equal(acceptParticipantComment(bucket, request('later')), true);
  assert.equal(bucket.get('Viewer::later').status, 'waiting');
});

test('viewers with the same name but different channels queue separately', () => {
  const bucket = new Map();
  replay(bucket, [{...request('a'), author_channel_id: 'UC1'}, {...request('b'), author_channel_id: 'UC2'}]);
  assert.deepEqual(statuses(bucket), [['a', 'waiting'], ['b', 'waiting']]);
});

test('a renamed viewer with a channel ID is still one person', () => {
  const bucket = new Map();
  replay(bucket, [{...request('a', 'Old'), author_channel_id: 'UC1'}, {...request('b', 'New'), author_channel_id: 'UC1'}]);
  assert.deepEqual(statuses(bucket), [['a', 'waiting'], ['b', 'deleted']]);
});

test('a handled comment stays handled after its author gets linked to a channel ID', () => {
  const bucket = new Map();
  replay(bucket, [request('a')]);
  bucket.get('Viewer::a').status = 'done';
  assert.equal(acceptParticipantComment(bucket, {...request('a'), author_channel_id: 'UC1'}), false);
  assert.equal(bucket.size, 1);
});

test('state restored from disk also remembers its comments', () => {
  const bucket = new Map([['Viewer::a', {id: 'a', author_key: 'Viewer::a', author_id: 'Viewer', status: 'done'}]]);
  assert.equal(acceptParticipantComment(bucket, {...request('a'), author_channel_id: 'UC1'}), false);
});

test('someone waiting under their name is matched once linked to a channel ID', () => {
  const bucket = new Map();
  replay(bucket, [request('a')]);
  replay(bucket, [{...request('b'), author_channel_id: 'UC1'}]);
  assert.equal(bucket.get('UC1::b').status, 'deleted');
});

test('ignores ordinary comments and non-text messages', () => {
  const bucket = new Map();
  assert.equal(acceptParticipantComment(bucket, {...request('x'), text: 'こんにちは'}), false);
  assert.equal(acceptParticipantComment(bucket, {...request('y'), kind: 'paid'}), false);
  assert.equal(bucket.size, 0);
});

const crypto = {
  isEncryptionAvailable: () => true,
  encryptString: value => Buffer.from(`sealed:${value}`),
  decryptString: buffer => buffer.toString().replace('sealed:', ''),
};

test('the OBS password is encrypted when saved and restored when read', () => {
  const sealed = sealPassword({host: 'h', password: 'secret'}, crypto);
  assert.equal('password' in sealed, false);
  assert.equal(JSON.stringify(sealed).includes('secret'), false);
  assert.equal(openPassword(sealed, crypto), 'secret');
});

test('OBS password handling covers legacy, unavailable and unreadable values', () => {
  assert.equal(openPassword({password: 'plain'}, crypto), 'plain');
  assert.deepEqual(sealPassword({password: ''}, crypto), {password: ''});
  const unavailable = {...crypto, isEncryptionAvailable: () => false};
  assert.deepEqual(sealPassword({password: 'plain'}, unavailable), {password: 'plain'});
  const broken = {...crypto, decryptString() { throw Error('other user'); }};
  const warn = console.warn; console.warn = () => {};
  try { assert.equal(openPassword({passwordEncrypted: 'AA=='}, broken), ''); } finally { console.warn = warn; }
});

test('an empty target folder is replaced without leaving a set-aside copy', t => {
  const appData = fs.mkdtempSync(path.join(require('node:os').tmpdir(), 'yuilivekit-'));
  t.after(() => fs.rmSync(appData, {recursive: true, force: true}));
  const target = path.join(appData, 'YuiLiveKit');
  fs.mkdirSync(target);
  fs.mkdirSync(path.join(appData, 'yt-overlay'));
  fs.writeFileSync(path.join(appData, 'yt-overlay', 'comments.db'), 'comments');

  const result = migrateUserData({appData, target});

  assert.equal(result.setAside, null);
  assert.equal(fs.readFileSync(path.join(target, 'comments.db'), 'utf8'), 'comments');
});
