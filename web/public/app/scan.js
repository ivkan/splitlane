// Reading the computer's QR code with the phone's camera, inside the page.
//
// A page added to an iPhone's home screen keeps its own storage, apart from
// Safari's, and the system camera opens a code's link in Safari. So the
// page pairs from its own camera. Browsers that have `BarcodeDetector`
// (Chrome on Android) read the code with it; the rest, Safari among them,
// with jsQR, kept in vendor/ and loaded only when a scan starts.

const FRAME_MS = 150;
/// Frames are scaled down to this width before decoding: a pairing code
/// fills the frame and reads as well, at a fraction of the work.
const DECODE_WIDTH = 640;

let jsQRLoaded = null;

function loadJsQR() {
  jsQRLoaded ??= new Promise((resolve, reject) => {
    const script = document.createElement("script");
    script.src = "vendor/jsQR.js";
    script.onload = () => (globalThis.jsQR ? resolve(globalThis.jsQR) : reject(new Error("the QR reader did not load")));
    script.onerror = () => {
      jsQRLoaded = null;
      reject(new Error("the QR reader did not load"));
    };
    document.head.append(script);
  });
  return jsQRLoaded;
}

async function decoder() {
  if ("BarcodeDetector" in globalThis) {
    try {
      if ((await BarcodeDetector.getSupportedFormats()).includes("qr_code")) {
        const detector = new BarcodeDetector({ formats: ["qr_code"] });
        return async (video) => (await detector.detect(video))[0]?.rawValue ?? null;
      }
    } catch {
      // Fall back on jsQR.
    }
  }
  const jsQR = await loadJsQR();
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d", { willReadFrequently: true });
  return async (video) => {
    if (!video.videoWidth) return null;
    const scale = Math.min(1, DECODE_WIDTH / video.videoWidth);
    canvas.width = Math.round(video.videoWidth * scale);
    canvas.height = Math.round(video.videoHeight * scale);
    context.drawImage(video, 0, 0, canvas.width, canvas.height);
    const image = context.getImageData(0, 0, canvas.width, canvas.height);
    return jsQR(image.data, image.width, image.height, { inversionAttempts: "dontInvert" })?.data ?? null;
  };
}

/// Show the back camera in `video` and call `onText` with each code read
/// until the returned `stop` is called. Rejects with words for a person
/// when there is no camera or no leave to use it.
export async function startScan(video, onText) {
  if (!navigator.mediaDevices?.getUserMedia) {
    throw new Error("This browser gives the page no camera.");
  }
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      video: { facingMode: { ideal: "environment" } },
      audio: false,
    });
  } catch (error) {
    if (error?.name === "NotAllowedError") {
      throw new Error("The camera was not allowed. Allow it for this page in the phone's settings, or paste the link instead.");
    }
    throw new Error("No camera could be opened.");
  }
  let stopped = false;
  const stop = () => {
    stopped = true;
    for (const track of stream.getTracks()) track.stop();
    video.srcObject = null;
  };
  try {
    video.setAttribute("playsinline", "");
    video.muted = true;
    video.srcObject = stream;
    await video.play();
    const read = await decoder();
    (async () => {
      while (!stopped) {
        let text = null;
        try {
          text = await read(video);
        } catch {
          // A frame that could not be read; the next one may.
        }
        if (stopped) return;
        if (text) onText(text);
        await new Promise((r) => setTimeout(r, FRAME_MS));
      }
    })();
  } catch (error) {
    stop();
    throw error;
  }
  return stop;
}
