// Verbatim preload.js from gregoreesmaa/strake-minimal-repro@main: stamps the
// runtime versions into the DOM on DOMContentLoaded (issue #145).
window.addEventListener('DOMContentLoaded', () => {
  const replaceText = (selector, text) => {
    const element = document.getElementById(selector)
    if (element) element.innerText = text
  }

  for (const type of ['chrome', 'node', 'electron']) {
    replaceText(`${type}-version`, process.versions[type])
  }
})
