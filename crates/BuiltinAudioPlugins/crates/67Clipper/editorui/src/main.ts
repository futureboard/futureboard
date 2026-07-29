import { mount } from 'svelte'

import '@fontsource-variable/mona-sans'
import './app.css'
import App from './App.svelte'

const root = document.getElementById('root')
if (!root) throw new Error('#root is missing from index.html')

export default mount(App, { target: root })
