# Your phone

A paired phone lists the agent sessions on your computer, shows each one's
state, its question when it is waiting for you and its last answer, and lets
you answer it: type a reply, press one of the keys a prompt asks for, or
interrupt a turn. The phone needs no app. It is a page at
<https://splitlane.dev/app>, which you can add to the home screen.

## Pairing

1. On the computer, open **Settings → Phone** and choose **Pair a phone**. A
   QR code appears; it lasts five minutes.
2. Scan it with the phone's camera and open the link. Give the phone a name and
   tap **Pair**.
3. The phone shows six digits. Type them on the computer. Only then does the
   computer keep the phone: whoever photographed the QR code would see a code
   on their own screen, not on yours.

From a terminal, `splitlane remote pair` does the same and prints the code as
text.

A page added to the home screen on an iPhone keeps its own storage, apart from
Safari's. To pair it, copy the link (under the QR code, or from `splitlane
remote pair`) and paste it under **Paste a pairing link** on the page.

## What the phone may do

- **Read:** the list of agent sessions, their state, the question an agent is
  asking, its last answer and the text on its screen.
- **Reply:** text into an agent session, the keys a prompt asks for (digits,
  `y`, `n`, `Enter`, `Esc`, the arrows), and interrupting a turn. Only agent
  sessions take it, never a plain shell. Each reply goes into a journal on the
  computer (time, phone, session, length; never the text), and while the
  window is open it shows a notice there naming the phone. Turn off **Can reply** in Settings → Phone to let a
  phone read only.

The phone reaches the computer while the window is open, and while it is
closed with sessions still running in the session host. Interrupting needs the
window.

## How it is connected

Neither side opens a port. Both keep an outgoing connection to
`relay.splitlane.dev`, which forwards messages between them. Every message is
encrypted end to end between a key on the computer and a key on the phone; the
relay sees only that a message passed, how long it was and when. The phone's
key is kept by the browser and cannot be copied out of it, even by the page.

If the browser clears the page's storage (iOS does when space runs short), the
phone is no longer paired: remove it on the computer and pair again.

## Removing a phone

**Remove** in Settings → Phone. From the next message on, the computer answers
it nothing. **Forget** on the phone, under `⋯`, drops its key there.
