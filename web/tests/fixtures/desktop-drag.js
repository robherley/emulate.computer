// Evaluate in a fresh Desktop view with Metrics open and the default terminal visible.
(async () => {
  const canvas = document.querySelector('.display-canvas');
  const metrics = document.querySelector('.stats-grid');
  if (!canvas || !metrics || !metrics.getClientRects().length) throw new Error('Open Desktop and Metrics first.');
  const deadline = performance.now() + 30000;
  while (document.querySelector('.desktop-cover')) {
    if (performance.now() > deadline) throw new Error('Desktop did not become ready');
    await new Promise(resolve => setTimeout(resolve, 100));
  }
  let latest = {};
  const stats = event => { latest = event.detail; };
  window.addEventListener('emulate-stats', stats);
  await new Promise(resolve => setTimeout(resolve, 1000));
  const box = canvas.getBoundingClientRect();
  const sample = document.createElement('canvas');
  sample.width = canvas.width;
  sample.height = canvas.height;
  const context = sample.getContext('2d', { willReadFrequently: true });
  context.drawImage(canvas, 0, 0);
  const pixels = context.getImageData(0, 0, sample.width, sample.height).data;
  const windowRows = pixels => {
    const lefts = new Map();
    for (let y = 0; y < sample.height; y++) {
      let run = 0;
      for (let x = 0; x <= sample.width; x++) {
        const at = (y * sample.width + x) * 4;
        if (x < sample.width && pixels[at] === 10 && pixels[at + 1] === 10 && pixels[at + 2] === 10) run++;
        else {
          if (run > 580) lefts.set(x - run, (lefts.get(x - run) ?? 0) + 1);
          run = 0;
        }
      }
    }
    return [...lefts].filter(([, rows]) => rows > 10).sort((a, b) => b[1] - a[1]);
  };
  const initial = windowRows(pixels);
  if (initial.length !== 1) throw new Error('Wait for the default terminal to finish drawing.');
  let title;
  for (let y = 0; y < sample.height && !title; y++) {
    let run = 0;
    for (let x = 0; x < sample.width; x++) {
      const at = (y * sample.width + x) * 4;
      run = pixels[at] === 37 && pixels[at + 1] === 37 && pixels[at + 2] === 37 ? run + 1 : 0;
      if (run > 300) { title = [x - run + 100, y + 10]; break; }
    }
  }
  if (!title) throw new Error('Cannot find the default terminal title bar.');
  if (title[0] > 300 || title[1] > 100) throw new Error('Reload for the default window position.');
  const send = (type, x, y, buttons) => canvas.dispatchEvent(new PointerEvent(type, {
    clientX: box.left + x / sample.width * box.width,
    clientY: box.top + y / sample.height * box.height,
    button: 0, buttons, pointerId: 1, bubbles: true,
  }));
  // Synthetic pointer events have no native capture target.
  const capture = canvas.setPointerCapture;
  canvas.setPointerCapture = () => {};
  const samples = [];
  let moves = 0;
  try {
    canvas.focus();
    send('pointermove', ...title, 0);
    await new Promise(resolve => setTimeout(resolve, 400));
    send('pointerdown', ...title, 1);
    const start = performance.now();
    let lastSample = start;
    await new Promise(resolve => {
      const tick = () => {
        const now = performance.now();
        const elapsed = now - start;
        const progress = elapsed < 3000 ? elapsed / 3000 : Math.max(0, 2 - elapsed / 3000);
        send('pointermove', title[0] + 300 * progress, title[1] + 240 * progress, 1);
        moves++;
        if (now - lastSample >= 250) {
          const values = metrics.innerText.split('\n');
          context.drawImage(canvas, 0, 0);
          const regions = windowRows(context.getImageData(0, 0, sample.width, sample.height).data);
          const expectedLeft = initial[0][0] + 300 * progress;
          samples.push({
            ms: Math.round(elapsed),
            frameP95: latest.displayFrameP95,
            workerSliceP95: latest.workerSliceP95,
            inputQueueP95: latest.inputQueueP95,
            inputToGuestP95: latest.inputToGuestP95,
            inputPendingEvents: latest.inputPendingEvents,
            executionPercent: latest.executionPercent,
            uploadBytesPerSec: latest.displayUploadBytesPerSec,
            backend: latest.displayBackend,
            committed: latest.displayCommitted,
            fps: Number(values[values.indexOf('Display') + 1]),
            mips: Number(values[values.indexOf('CPU') + 1]),
            expectedLeft, windowLeft: regions[0]?.[0], regions: regions.length,
          });
          lastSample = now;
        }
        if (elapsed < 6000) requestAnimationFrame(tick);
        else resolve();
      };
      requestAnimationFrame(tick);
    });
  } finally {
    send('pointerup', ...title, 0);
    canvas.setPointerCapture = capture;
    window.removeEventListener('emulate-stats', stats);
  }
  const steady = samples.filter(sample => sample.ms >= 1250);
  const tracked = steady.filter(sample => sample.windowLeft !== undefined);
  const errors = tracked.map(sample => Math.abs(sample.windowLeft - sample.expectedLeft));
  return {
    title, moves,
    backend: latest.displayBackend,
    committed: latest.displayCommitted,
    frameP95Ms: latest.displayFrameP95,
    maxWorkerSliceP95Ms: Math.max(...steady.map(s => s.workerSliceP95 ?? 0)),
    maxInputQueueP95Ms: Math.max(...steady.map(s => s.inputQueueP95 ?? 0)),
    maxInputToGuestP95Ms: Math.max(...steady.map(s => s.inputToGuestP95 ?? 0)),
    maxInputPendingEvents: Math.max(...steady.map(s => s.inputPendingEvents ?? 0)),
    meanExecutionPercent: steady.reduce((sum, sample) => sum + (sample.executionPercent ?? 0), 0) / steady.length,
    meanUploadBytesPerSec: steady.reduce((sum, sample) => sum + (sample.uploadBytesPerSec ?? 0), 0) / steady.length,
    estimatedMotionLagP95Ms: [...errors].sort((a,b) => a-b)[Math.ceil(errors.length * .95) - 1] / 0.1,
    positionSamples: tracked.length,
    tornSamples: tracked.filter(sample => sample.regions > 1).length,
    meanPositionErrorPx: errors.reduce((sum, error) => sum + error, 0) / errors.length,
    maxPositionErrorPx: Math.max(...errors),
    meanPaintedFps: steady.reduce((sum, sample) => sum + sample.fps, 0) / steady.length,
    meanMips: steady.reduce((sum, sample) => sum + sample.mips, 0) / steady.length,
    samples,
  };
})()
