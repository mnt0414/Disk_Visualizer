import { fireEvent,render,screen,waitFor } from '@testing-library/react'
import { beforeEach,describe,expect,it,vi } from 'vitest'
import { chooseFolder } from '../services/folderPicker'
import { startScan } from '../services/scan'
import { MockView } from './MockView'
vi.mock('../services/folderPicker',()=>({chooseFolder:vi.fn()}))
vi.mock('../services/scan',()=>({startScan:vi.fn(),getScanStatus:vi.fn(),pauseScan:vi.fn(),resumeScan:vi.fn(),cancelScan:vi.fn()}))
const mockedChooseFolder=vi.mocked(chooseFolder);const mockedStartScan=vi.mocked(startScan)
beforeEach(()=>{mockedChooseFolder.mockReset();mockedStartScan.mockReset()})
const result={rootPath:'/Users/test/Documents',totalSizeBytes:1024,fileCount:1,directoryCount:0,skippedCount:0,elapsedMilliseconds:2,entries:[{name:'note.txt',path:'/Users/test/Documents/note.txt',sizeBytes:1024,fileCount:1,directoryCount:0,skippedCount:0,isDirectory:false}]}
describe('MockView',()=>{it('renders application cache evidence',()=>{render(<MockView viewId="app-cache" label="アプリキャッシュ" description=""/>);expect(screen.getByRole('heading',{name:'キャッシュ候補'})).toBeInTheDocument()});it('opens the native folder picker',async()=>{mockedChooseFolder.mockResolvedValue(null);render(<MockView viewId="scan" label="スキャン" description=""/>);fireEvent.click(screen.getByRole('button',{name:'フォルダを選択'}));await waitFor(()=>expect(mockedChooseFolder).toHaveBeenCalledOnce())});it('starts an asynchronous scan and renders its result',async()=>{mockedStartScan.mockResolvedValue({id:1,path:'/Users/test/Documents',status:'completed',currentPath:'',totalSizeBytes:1024,fileCount:1,directoryCount:0,skippedCount:0,result,error:null});render(<MockView viewId="scan" label="スキャン" description=""/>);const input=screen.getByLabelText('スキャン対象');fireEvent.change(input,{target:{value:'/Users/test/Documents'}});fireEvent.submit(input.closest('form')!);await waitFor(()=>expect(screen.getByText('note.txt')).toBeInTheDocument());expect(mockedStartScan).toHaveBeenCalledWith('/Users/test/Documents')})})
