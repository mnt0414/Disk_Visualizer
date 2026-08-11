export type ScanEntry={name:string;path:string;sizeBytes:number;allocatedSizeBytes:number;fileCount:number;directoryCount:number;skippedCount:number;hardLinkDuplicateCount:number;sparseFileCount:number;compressedFileCount:number;isDirectory:boolean}
export type ScanSummary={rootPath:string;totalSizeBytes:number;allocatedSizeBytes:number;fileCount:number;directoryCount:number;skippedCount:number;hardLinkDuplicateCount:number;sparseFileCount:number;compressedFileCount:number;elapsedMilliseconds:number;entries:ScanEntry[];entriesTruncated:boolean}
export type ScanJobStatus='running'|'paused'|'completed'|'cancelled'|'failed'
export type ScanJobSnapshot={id:number;path:string;status:ScanJobStatus;currentPath:string;totalSizeBytes:number;fileCount:number;directoryCount:number;skippedCount:number;result:ScanSummary|null;error:string|null}
export type SavedScan={id:number;rootPath:string;totalSizeBytes:number;fileCount:number;directoryCount:number;skippedCount:number;completedAt:number}
