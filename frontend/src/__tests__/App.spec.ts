import { describe, it, expect } from 'vitest'

import { mount } from '@vue/test-utils'
import App from '../App.vue'

describe('App', () => {
  it('mounts and renders the sidebar shell', () => {
    const wrapper = mount(App)
    expect(wrapper.text()).toContain('Acme Inc')
    expect(wrapper.text()).toContain('Home')
  })
})
