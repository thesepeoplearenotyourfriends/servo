const canvas = document.getElementById('canvas');
const ctx = canvas.getContext('2d');
const log = document.getElementById('log');
let pointer = { x: 40, y: 80 };
let frame = 0;

function append(message) {
  log.textContent += `${new Date().toLocaleTimeString()} ${message}\n`;
}

function draw() {
  frame += 1;
  ctx.clearRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = '#0f172a';
  ctx.fillRect(0, 0, canvas.width, canvas.height);
  ctx.fillStyle = '#38bdf8';
  const x = 30 + ((frame * 2) % (canvas.width - 60));
  ctx.beginPath();
  ctx.arc(x, 50, 18, 0, Math.PI * 2);
  ctx.fill();
  ctx.fillStyle = '#f59e0b';
  ctx.beginPath();
  ctx.arc(pointer.x, pointer.y, 12, 0, Math.PI * 2);
  ctx.fill();
  ctx.fillStyle = '#e2e8f0';
  ctx.fillText('Canvas animation continues while Python delays replies.', 20, 135);
  requestAnimationFrame(draw);
}
requestAnimationFrame(draw);

canvas.addEventListener('pointermove', (event) => {
  const rect = canvas.getBoundingClientRect();
  pointer = { x: event.clientX - rect.left, y: event.clientY - rect.top };
});
canvas.addEventListener('click', () => canvas.focus());
canvas.addEventListener('keydown', (event) => append(`canvas key: ${event.key}`));

document.getElementById('immediate').addEventListener('click', async () => {
  append('sending immediate request');
  const reply = await severin.send({ kind: 'immediate', time: Date.now() });
  append(`immediate reply: ${JSON.stringify(reply)}`);
});

document.getElementById('deferred').addEventListener('click', async () => {
  append('sending deferred request; animation should continue');
  const reply = await severin.send({ kind: 'deferred', delayMs: 2500, time: Date.now() });
  append(`deferred reply: ${JSON.stringify(reply)}`);
});
