import { beforeEach, describe, expect, it } from 'vitest'
import { mount } from '@vue/test-utils'
import MarkdownDocument from '@/components/markdown/MarkdownDocument.vue'
import { useBlockSelection } from '@/composables/useBlockSelection'
import { useBlockInteraction } from '@/composables/useBlockInteraction'
import { useBlockComments } from '@/composables/useBlockComments'
import type { MarkdownBlock } from '@/markdown/types'

const blocks: MarkdownBlock[] = [
  { id: 'heading-1', type: 'heading', order: 1, depth: 1, text: 'Title' },
  { id: 'paragraph-1', type: 'paragraph', order: 2, text: '<script>alert("x")</script>' },
  { id: 'code-1', type: 'code', order: 3, lang: 'ts', raw: 'const value = 1' },
  {
    id: 'table-1',
    type: 'table',
    order: 4,
    meta: { table: { headers: ['Name'], rows: [['Qingluan']] } },
  },
]

describe('MarkdownDocument', () => {
  beforeEach(() => {
    useBlockSelection().clearSelection()
    useBlockInteraction().reset()
    useBlockComments().reset()
  })

  it('renders each block with data-block-id and text content', () => {
    const wrapper = mount(MarkdownDocument, { props: { blocks } })

    expect(wrapper.find('[data-block-id="heading-1"]').exists()).toBe(true)
    expect(wrapper.find('[data-block-id="paragraph-1"]').exists()).toBe(true)
    expect(wrapper.text()).toContain('Title')
    expect(wrapper.text()).toContain('const value = 1')
    expect(wrapper.text()).toContain('Qingluan')
  })

  it('renders unsafe text as text instead of html', () => {
    const wrapper = mount(MarkdownDocument, { props: { blocks } })

    expect(wrapper.find('script').exists()).toBe(false)
    expect(wrapper.text()).toContain('<script>alert("x")</script>')
  })

  it('selects a block when its frame is clicked', async () => {
    const wrapper = mount(MarkdownDocument, { props: { blocks } })

    await wrapper.find('[data-block-id="heading-1"]').trigger('click')

    expect(useBlockSelection().selectedBlockIds.value.has('heading-1')).toBe(true)
  })

  it('opens a comment popup at click and stores the comment on submit, showing a bubble', async () => {
    const wrapper = mount(MarkdownDocument, { props: { blocks }, attachTo: document.body })

    // click the block -> toolbar appears (teleported to body)
    await wrapper.find('[data-block-id="heading-1"]').trigger('click')
    await document.querySelector('[data-testid="comment-heading-1"]')?.dispatchEvent(new Event('click', { bubbles: true }))

    // comment input popup should now exist on body
    const input = document.querySelector('[data-testid="comment-input-heading-1"]') as HTMLTextAreaElement | null
    expect(input).toBeTruthy()
    input!.value = 'looks good'
    input!.dispatchEvent(new Event('input', { bubbles: true }))
    await wrapper.vm.$nextTick()

    // submit
    document.querySelector('[data-testid="comment-submit-heading-1"]')?.dispatchEvent(new Event('click', { bubbles: true }))
    await wrapper.vm.$nextTick()

    // bubble with count should now be rendered in the document frame
    const bubble = wrapper.find('[data-testid="bubble-heading-1"]')
    expect(bubble.exists()).toBe(true)
    expect(bubble.text()).toContain('1')

    // open the bubble -> comment item text appears
    await bubble.trigger('click')
    await wrapper.vm.$nextTick()
    const item = document.querySelector('[data-testid="comment-item-heading-1"]')
    expect(item?.textContent).toContain('looks good')

    wrapper.unmount()
  })
})
