// Set window.desktopWorkload to scroll, dillo, overlap, or cpu-drag; use ?benchmark=1.
(async () => {
  const kind = window.desktopWorkload ?? 'scroll';
  const tab = name => [...document.querySelectorAll('[role=tab]')].find(tab => tab.textContent === name);
  const commands = {
    scroll: "DISPLAY=:0 xterm -geometry 90x30+20+20 -e sh -c 'i=0; while [ $i -lt 4000 ]; do printf \"line %s: terminal scrolling benchmark\\n\" \"$i\"; i=$((i+1)); done; sleep 3' &",
    dillo: "{ echo '<html><body>'; i=0; while [ $i -lt 60 ]; do echo \"<p>Paragraph $i: Desktop scrolling, text layout, and repaint benchmark.</p>\"; i=$((i+1)); done; echo '</body></html>'; } > /tmp/emulate-benchmark.html; DISPLAY=:0 dillo /tmp/emulate-benchmark.html &",
    overlap: "DISPLAY=:0 xterm -geometry 60x18+200+200 -e sh -c 'echo Overlapping window; sleep 15' &",
    'cpu-drag': "timeout 15 sh -c 'while :; do :; done' &",
  };
  if (!commands[kind]) throw new Error('Unknown desktop workload');
  const deadline = performance.now() + 30000;
  while (document.querySelector('.desktop-cover')) {
    if (performance.now() > deadline) throw new Error('Desktop did not become ready');
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  tab('Console').click();
  await new Promise(resolve => setTimeout(resolve, 100));
  const clipboard = new DataTransfer(); clipboard.setData('text/plain', commands[kind] + '\n');
  const terminal = document.querySelector('.xterm-helper-textarea');
  terminal.dispatchEvent(new ClipboardEvent('paste', { clipboardData: clipboard, bubbles: true }));
  await new Promise(resolve => setTimeout(resolve, 200));
  tab('Desktop').click();
  await new Promise(resolve => requestAnimationFrame(resolve));
  const samples = [];
  const receive = event => samples.push({ at: performance.now(), ...event.detail });
  window.addEventListener('emulate-stats', receive);
  const launched = performance.now();
  const canvas = document.querySelector('.display-canvas');
  const bounds = canvas.getBoundingClientRect();
  canvas.dispatchEvent(new PointerEvent('pointermove', {clientX: bounds.left + bounds.width / 2, clientY: bounds.top + bounds.height / 2, bubbles: true}));
  if (kind === 'dillo') {
    const probe = document.createElement('canvas'); probe.width = 64; probe.height = 48;
    const context = probe.getContext('2d', {willReadFrequently: true});
    for (;;) {
      context.drawImage(canvas, 0, 0, 64, 48);
      const pixels = context.getImageData(0, 0, 64, 48).data;
      let light = 0;
      for (let i = 0; i < pixels.length; i += 4)
        if (pixels[i] > 120 && pixels[i + 1] > 120 && pixels[i + 2] > 100) light++;
      const recent = samples.slice(-8);
      if (light > 64 * 48 * .2 && recent.length === 8 && recent.every(s => s.executionPercent < 50)) break;
      if (performance.now() - launched > 60000) throw new Error('Dillo did not finish its first paint');
      await new Promise(resolve => setTimeout(resolve, 250));
    }
  }
  canvas.focus();
  canvas.dispatchEvent(new PointerEvent('pointermove', {clientX: bounds.left + bounds.width / 2, clientY: bounds.top + bounds.height / 2, bubbles: true}));
  const startupMs = performance.now() - launched;
  samples.length = 0;
  const start = performance.now();
  let wheel = 0;
  try {
    if (kind === 'dillo') wheel = setInterval(() => canvas.dispatchEvent(new WheelEvent('wheel', {deltaY: (Math.floor((performance.now() - start) / 3000) % 2 ? -120 : 120), bubbles: true, cancelable: true})), 150);
    if (kind === 'overlap' || kind === 'cpu-drag') {
      await new Promise(resolve => setTimeout(resolve, kind === 'overlap' ? 3000 : 1000));
      const drag = await (0, eval)(window.desktopDragSource ?? await (await fetch('/tests/fixtures/desktop-drag.js')).text());
      return {workload: kind, drag};
    }
    await new Promise(resolve => setTimeout(resolve, 12000));
  } finally {
    clearInterval(wheel);
    window.removeEventListener('emulate-stats', receive);
  }
  if (samples.length < 20) throw new Error('No benchmark telemetry: add ?benchmark=1 to the URL');
  const steady = samples.filter(s => s.at - start > 2000);
  const first = steady[0], last = steady.at(-1);
  return {
    workload: kind, startupMs, backend: last.displayBackend, committed: last.displayCommitted,
    paintedFps: (last.displayFrames - first.displayFrames) / (last.at - first.at) * 1000,
    frameP95Ms: Math.max(...steady.map(s => s.displayFrameP95)),
    executionPercent: steady.reduce((sum,s) => sum + s.executionPercent, 0) / steady.length,
    samples,
    uploadBytesPerSec: steady.reduce((sum,s) => sum + s.displayUploadBytesPerSec, 0) / steady.length,
  };
})()
