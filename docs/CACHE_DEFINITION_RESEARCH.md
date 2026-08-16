# Cache definition research

## Decision rule

Add a fixed cache definition only when vendor documentation identifies a stable default path and the content is described as regenerable cache data. If the path belongs to a project, can contain source media, or is selected through application settings, defer classification until Disk Visualizer can read that configuration explicitly.

## Adobe media cache

Status: fixed default paths are eligible.

Adobe documents the shared Premiere media cache under these defaults:

- macOS: `~/Library/Application Support/Adobe/Common`
- Windows: `%APPDATA%\Adobe\Common`

The catalog classifies only the documented `Media Cache Files` and `Media Cache` children, not the entire `Adobe/Common` directory. Adobe also allows the cache location to be changed in application settings; custom locations are not detected by the fixed rules in this version.

Cleanup impact: cached media and its database can be recreated, but the next import, playback, or conform operation may be slower. Users should close Adobe applications and prefer their built-in cache management commands before manual cleanup.

Source:

- https://helpx.adobe.com/premiere/desktop/troubleshooting/media-issues/delete-media-cache-files-manually.html
- https://helpx.adobe.com/premiere/desktop/troubleshooting/media-issues/manage-media-cache.html

## Blender asset library cache

Status: fixed paths are eligible.

Blender documents a local cache used for persistent Asset Library indexing:

- macOS: `/Library/Caches/Blender/`
- Windows: `%USERPROFILE%\AppData\Local\Blender Foundation\Blender\Cache\`

This is distinct from Blender's temporary directory and from project simulation caches. The catalog classifies only the documented local Asset Library cache. It does not classify temporary render layers, physics caches, autosaves, project files, or configurable external caches.

Cleanup impact: Asset Library indexes may need to be rebuilt and the next asset search may take longer.

Source:

- https://docs.blender.org/manual/en/latest/advanced/blender_directory_layout.html

## DaVinci Resolve

Status: deferred; do not add a fixed path.

DaVinci Resolve keeps Proxy, Cache, and Gallery locations in Project Settings > Master Settings > Working Folders. Defaults also depend on the first Media Storage location, and Resolve 20 adds project-specific generated media locations. A broad fixed path could mix regenerable cache with project-generated media.

Future implementation should read Resolve project or user configuration and classify the exact configured cache directory with `source: configuration` or `source: userSpecified`.

Source:

- https://documents.blackmagicdesign.com/SupportNotes/DaVinci_Resolve_20_New_Features_Guide.pdf

## Autodesk Flame

Status: deferred; do not add a fixed path.

Flame media cache is associated with Project Home and can be moved to dedicated or shared storage. The configured path must be consistent across hosts for shared projects. A fixed system path would be unsafe because the location is project- and environment-specific.

Future implementation should read Flame project configuration and distinguish Media Cache from project metadata and source media before classifying it.

Source:

- https://help.autodesk.com/view/FLAME/2027/ENU/?guid=project-sharing
