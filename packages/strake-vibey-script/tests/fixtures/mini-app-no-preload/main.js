const { app, BrowserWindow } = require('electron')

function createWindow () {
  // No webPreferences.preload: the IPC proof still needs a renderer.
  const mainWindow = new BrowserWindow({
    width: 800,
    height: 600
  })

  mainWindow.loadFile('index.html')
}

app.whenReady().then(() => {
  createWindow()
})

app.on('window-all-closed', function () {
  if (process.platform !== 'darwin') app.quit()
})
