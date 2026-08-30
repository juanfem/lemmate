import { mount } from 'svelte'
import 'katex/dist/katex.min.css'
import './app.css'
import App from './App.svelte'

mount(App, { target: document.getElementById('app')! })
