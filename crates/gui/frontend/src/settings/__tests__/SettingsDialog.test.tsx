import { act, useState } from 'react'
import {createRoot} from 'react-dom/client'
import {expect,it,vi} from 'vitest'
import type {AppConfigDto} from '@/types'
import {SettingsDialog} from '../SettingsDialog'
import {saveConfig} from '@/api'
(globalThis as any).IS_REACT_ACT_ENVIRONMENT=true
vi.mock('@/api',()=>({getConfig:vi.fn(async()=>config()),saveConfig:vi.fn(async()=>{throw {kind:'internal',message:'fixture write denied'}})}))
vi.mock('@/components/ui/dialog',()=>({Dialog:({open,onOpenChange,children}:any)=>open?<div><button id="close" onClick={()=>onOpenChange(false)}>close</button>{children}</div>:null,DialogContent:({children}:any)=><div>{children}</div>,DialogDescription:({children}:any)=><div>{children}</div>,DialogHeader:({children}:any)=><div>{children}</div>,DialogTitle:({children}:any)=><div>{children}</div>}))
vi.mock('../tabs/GeneralTab',()=>({GeneralTab:({draft,onChange}:any)=><button id="edit" onClick={()=>onChange({...draft,general:{...draft.general,refresh_interval_secs:99}},false)}>edit {draft.general.refresh_interval_secs}</button>}))
function config(refreshIntervalSecs = 30): AppConfigDto {
  return {
    general: { refresh_interval_secs: refreshIntervalSecs, auto_fetch: false },
    repositories: [],
    scan_roots: [],
    github: { token_env: null, enabled: false, enable_issues: true, enable_prs: true },
    notifications: { enabled: false, cooldown_secs: 300 },
    gui: {
      theme: 'dark',
      start_in_tray: false,
      hide_from_taskbar: false,
      close_to_tray: true,
      global_shortcut: null,
      window_x: null,
      window_y: null,
      window_width: null,
      window_height: null,
      window_maximized: null,
      column_widths: {},
    },
  }
}


it('retains failed close drafts and lets close retry successfully',async()=>{
 const container=document.createElement('div');document.body.appendChild(container);const root=createRoot(container);
 function Harness(){const [open,setOpen]=useState(true);return <SettingsDialog open={open} onOpenChange={setOpen}/>}
 await act(async()=>{root.render(<Harness/>)});
 await act(async()=>{(container.querySelector('#edit') as HTMLElement).click()});
 expect(container.textContent).toContain('edit 99');
 await act(async()=>{(container.querySelector('#close') as HTMLElement).click()});
 expect(saveConfig).toHaveBeenCalled();
 expect(container.textContent).toContain('edit 99');
 expect(container.textContent).toContain('fixture write denied');
 vi.mocked(saveConfig).mockResolvedValueOnce(undefined);
 await act(async()=>{(container.querySelector('#close') as HTMLElement).click()});
 expect(container.textContent).toBe('');
 await act(async()=>root.unmount());container.remove();
})
