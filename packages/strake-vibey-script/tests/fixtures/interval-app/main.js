const { app, BrowserWindow } = require('electron')

function createWindow () {
  const mainWindow = new BrowserWindow({
    width: 800,
    height: 600
  })

  mainWindow.loadFile('index.html')
}

// Joplin's waitForElectronAppReady pattern: poll app readiness on a timer
// instead of relying on the ready event alone. Without timer pumping the
// window never appears.
const poll = setInterval(() => {
  if (app.isReady()) {
    clearInterval(poll)
    createWindow()
  }
}, 10)
