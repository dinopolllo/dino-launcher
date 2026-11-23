//! Instalar una versión de Minecraft con su loader.

use crate::model::{allowed, maven_path, AssetIndex, Features, LoaderProfile, VersionJson, VersionManifest};
use crate::{io_err, java, Error, Paths, Report, Result};
use dino_download::{Downloader, Item, Stats};
use std::path::{Path, PathBuf};

const MOJANG_MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";
const RESOURCES: &str = "https://resources.download.minecraft.net";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2/versions/loader";
/// Perfiles de NeoForge ya preprocesados (librerías con URL y hash, processors y sus datos).
/// Es un servicio de Modrinth: ver «Perfiles de NeoForge» en el documento de la etapa 2.
const NEOFORGE_META: &str = "https://launcher-meta.modrinth.com/neo/v0/versions";
const GAME_VERSION_PLACEHOLDER: &str = "${modrinth.gameVersion}";

/// El loader de una instancia: el mismo tipo que guarda `instance.json`.
pub use dino_core::schema::LoaderSpec as Loader;

#[derive(Debug, Clone)]
pub struct Installed {
    pub version: VersionJson,
    pub java: PathBuf,
    pub client_jar: PathBuf,
}

/// Cifras de la instalación, para el informe de rendimiento.
#[derive(Debug, Clone, Default)]
pub struct InstallStats {
    pub libraries: Stats,
    pub assets: Stats,
    pub java: bool,
    pub processors_run: usize,
}

pub async fn install(
    dl: &Downloader,
    dirs: &Paths,
    mc: &str,
    loader: &Loader,
    report: Report<'_>,
) -> Result<(Installed, InstallStats)> {
    report("Leyendo versiones de Minecraft", None);
    let manifest: VersionManifest = dl.json(MOJANG_MANIFEST).await?;
    let entry = manifest
        .versions
        .iter()
        .find(|v| v.id == mc)
        .ok_or_else(|| Error::Other(format!("Mojang no tiene la versión {mc}")))?;
    let vanilla: VersionJson = dl.json(&entry.url).await?;

    let version = match loader {
        Loader::Vanilla => vanilla.clone(),
        Loader::Fabric { version: v } => {
            let p: LoaderProfile = dl.json(&format!("{FABRIC_META}/{mc}/{v}/profile/json")).await?;
            merge(vanilla.clone(), p, mc)
        }
        Loader::NeoForge { version: v } => {
            let p: LoaderProfile = dl.json(&format!("{NEOFORGE_META}/{v}.json")).await?;
            merge(vanilla.clone(), p, mc)
        }
    };

    // El id con que se guarda lo calcula Dino, no el perfil: así `installed` lo encuentra
    // sin red aunque un loader cambie la forma de su id. Hoy coinciden (comprobado con los
    // perfiles de Fabric y de NeoForge).
    let mut version = version;
    version.id = version_id(mc, loader);
    let vdir = dirs.version_dir(&version.id);
    std::fs::create_dir_all(&vdir).map_err(io_err(&vdir))?;
    // El jar del cliente lleva el nombre de la versión instalada: NeoForge lo excluye de su
    // capa de módulos con `-DignoreList=…,${version_name}.jar`.
    let client_jar = vdir.join(format!("{}.jar", version.id));

    // Librerías + cliente, assets y Java a la vez: son servidores distintos.
    let libs = library_items(dirs, &version, &client_jar)?;
    let (libs_stats, assets_stats, java_bin) = tokio::try_join!(
        async { Ok::<_, Error>(dl.fetch_all(libs, &|p| report("Librerías", Some(p))).await?) },
        install_assets(dl, dirs, &vanilla, report),
        async {
            let component =
                vanilla.java_version.as_ref().map(|j| j.component.clone()).unwrap_or_else(|| "jre-legacy".into());
            java::ensure(dl, dirs, &component, report).await
        },
    )?;

    let processors_run = run_processors(dirs, &version, &client_jar, mc, &java_bin, report).await?;

    let json_path = vdir.join(format!("{}.json", version.id));
    std::fs::write(&json_path, serde_json::to_vec_pretty(&version)?).map_err(io_err(&json_path))?;

    Ok((
        Installed { version, java: java_bin, client_jar },
        InstallStats { libraries: libs_stats, assets: assets_stats, java: true, processors_run },
    ))
}

/// Id de la versión instalada para `mc` + `loader`: el `id` que dan los perfiles de cada
/// loader, y con el que se guardan el JSON fusionado y el jar del cliente.
pub fn version_id(mc: &str, loader: &Loader) -> String {
    match loader {
        Loader::Vanilla => mc.to_string(),
        Loader::Fabric { version } => format!("fabric-loader-{version}-{mc}"),
        Loader::NeoForge { version } => format!("neoforge-{version}"),
    }
}

/// Lo ya instalado, sin red: lo que hace falta para lanzar a partir de `instance.json`.
pub fn installed(dirs: &Paths, mc: &str, loader: &Loader) -> Result<Installed> {
    let id = version_id(mc, loader);
    let vdir = dirs.version_dir(&id);
    let json = vdir.join(format!("{id}.json"));
    let raw = std::fs::read(&json).map_err(io_err(&json))?;
    let version: VersionJson = serde_json::from_slice(&raw)?;
    let component = version.java_version.as_ref().map(|j| j.component.clone()).unwrap_or_else(|| "jre-legacy".into());
    let java = java::bin_for(dirs, &component);
    if !java.exists() {
        return Err(Error::Other(format!("falta Java ({component}): hay que reparar la instancia")));
    }
    Ok(Installed { client_jar: vdir.join(format!("{id}.jar")), version, java })
}

/// Fusiona el perfil de un loader con la versión de Mojang de la que hereda.
///
/// Las librerías del loader sustituyen a las de Mojang con el mismo `grupo:artefacto`
/// (traen versiones más nuevas de ASM, por ejemplo) y van **antes** en el classpath.
/// Adaptado de `merge_partial_version` de app-lib.
pub fn merge(mut v: VersionJson, p: LoaderProfile, mc: &str) -> VersionJson {
    let mut libs: Vec<_> = p
        .libraries
        .into_iter()
        .map(|mut l| {
            l.name = l.name.replace(GAME_VERSION_PLACEHOLDER, mc);
            l
        })
        .collect();
    let keys: std::collections::HashSet<String> =
        libs.iter().filter(|l| l.include_in_classpath).map(|l| l.key()).collect();
    v.libraries.retain(|l| !keys.contains(&l.key()));
    libs.extend(v.libraries);
    v.libraries = libs;

    if let Some(a) = p.arguments {
        let m = v.arguments.get_or_insert_with(Default::default);
        m.game.extend(a.game);
        m.jvm.extend(a.jvm);
    }
    v.id = p.id.replace(GAME_VERSION_PLACEHOLDER, mc);
    if let Some(mc) = p.main_class {
        v.main_class = mc;
    }
    v.data = p.data;
    v.processors = p.processors;
    v
}

fn library_items(dirs: &Paths, v: &VersionJson, client_jar: &Path) -> Result<Vec<Item>> {
    let f = Features::default();
    let mut items = Vec::new();
    if let Some(d) = &v.downloads {
        items.push(Item {
            url: d.client.url.clone(),
            dest: client_jar.to_path_buf(),
            sha1: d.client.sha1.clone(),
            size: d.client.size,
        });
    }
    for l in &v.libraries {
        if !l.downloadable || !allowed(l.rules.as_deref(), &f) {
            continue;
        }
        let (Some(path), Some(url)) = (l.path(), l.url()) else { continue };
        items.push(Item { url, dest: dirs.libraries().join(path), sha1: l.sha1(), size: l.size() });
    }
    Ok(items)
}

async fn install_assets(dl: &Downloader, dirs: &Paths, v: &VersionJson, report: Report<'_>) -> Result<Stats> {
    let Some(idx) = &v.asset_index else { return Ok(Stats::default()) };
    let index_path = dirs.assets().join("indexes").join(format!("{}.json", idx.id));
    dl.fetch_all(
        vec![Item { url: idx.url.clone(), dest: index_path.clone(), sha1: idx.sha1.clone(), size: idx.size }],
        &|_| {},
    )
    .await?;
    let raw = std::fs::read(&index_path).map_err(io_err(&index_path))?;
    let index: AssetIndex = serde_json::from_slice(&raw)?;
    let mut seen = std::collections::HashSet::new();
    let items: Vec<Item> = index
        .objects
        .values()
        .filter(|o| seen.insert(o.hash.clone()))
        .map(|o| {
            let hh = &o.hash[..2];
            Item {
                url: format!("{RESOURCES}/{hh}/{}", o.hash),
                dest: dirs.assets().join("objects").join(hh).join(&o.hash),
                sha1: Some(o.hash.clone()),
                size: Some(o.size),
            }
        })
        .collect();
    Ok(dl.fetch_all(items, &|p| report("Assets", Some(p))).await?)
}

// ---------------------------------------------------------------- processors

/// Ejecuta los processors de cliente de NeoForge (parchean el jar del juego).
/// Adaptado de `install_minecraft_inner` de app-lib.
async fn run_processors(
    dirs: &Paths,
    v: &VersionJson,
    client_jar: &Path,
    mc: &str,
    javaw: &Path,
    report: Report<'_>,
) -> Result<usize> {
    let (Some(procs), Some(data)) = (&v.processors, &v.data) else { return Ok(0) };
    let marker = dirs.version_dir(&v.id).join(".processors-ok");
    if marker.exists() {
        return Ok(0);
    }
    let libdir = dirs.libraries();
    let java = java::java_console(javaw);
    let client: Vec<_> =
        procs.iter().filter(|p| p.sides.as_ref().is_none_or(|s| s.iter().any(|x| x == "client"))).collect();

    let resolve = |arg: &str| -> Result<String> {
        if let Some(key) = arg.strip_prefix('{').and_then(|a| a.strip_suffix('}')) {
            let val = match key {
                "SIDE" => return Ok("client".into()),
                "MINECRAFT_JAR" => return Ok(client_jar.to_string_lossy().into_owned()),
                "MINECRAFT_VERSION" => return Ok(mc.into()),
                "ROOT" => return Ok(dirs.version_dir(&v.id).to_string_lossy().into_owned()),
                "LIBRARY_DIR" => return Ok(libdir.to_string_lossy().into_owned()),
                k => data
                    .get(k)
                    .map(|d| d.client.clone())
                    .ok_or_else(|| Error::Other(format!("processor pide {{{k}}} y el perfil no lo trae")))?,
            };
            return literal_or_lib(&val, &libdir);
        }
        literal_or_lib(arg, &libdir)
    };

    for (i, p) in client.iter().enumerate() {
        report(&format!("Preparando NeoForge ({}/{})", i + 1, client.len()), None);
        let jar = lib_path(&libdir, &p.jar)?;
        let mut cp: Vec<PathBuf> = p.classpath.iter().map(|c| lib_path(&libdir, c)).collect::<Result<_>>()?;
        cp.push(jar.clone());
        let sep = if cfg!(windows) { ";" } else { ":" };
        let cp = cp.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(sep);
        let main = main_class(&jar)?;
        let args: Vec<String> = p.args.iter().map(|a| resolve(a)).collect::<Result<_>>()?;

        let mut cmd = tokio::process::Command::new(&java);
        cmd.arg("-cp").arg(&cp).arg(&main).args(&args);
        #[cfg(windows)]
        {
            cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        let out = cmd.output().await.map_err(io_err(&java))?;
        if !out.status.success() {
            return Err(Error::Other(format!(
                "falló el processor {} ({main}): {}",
                p.jar,
                String::from_utf8_lossy(&out.stderr).lines().last().unwrap_or("")
            )));
        }
    }
    std::fs::write(&marker, b"").map_err(io_err(&marker))?;
    Ok(client.len())
}

fn lib_path(libdir: &Path, coord: &str) -> Result<PathBuf> {
    maven_path(coord).map(|p| libdir.join(p)).ok_or_else(|| Error::Other(format!("coordenada Maven inválida: {coord}")))
}

/// Valores de `data`: `[coord]` es una librería, `'texto'` es un literal.
fn literal_or_lib(v: &str, libdir: &Path) -> Result<String> {
    if let Some(c) = v.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        return Ok(lib_path(libdir, c)?.to_string_lossy().into_owned());
    }
    if let Some(l) = v.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')) {
        return Ok(l.into());
    }
    Ok(v.into())
}

fn main_class(jar: &Path) -> Result<String> {
    use std::io::Read;
    let f = std::fs::File::open(jar).map_err(io_err(jar))?;
    let mut z = zip::ZipArchive::new(f)?;
    let mut s = String::new();
    z.by_name("META-INF/MANIFEST.MF")?.read_to_string(&mut s).map_err(io_err(jar))?;
    s.lines()
        .find_map(|l| l.strip_prefix("Main-Class:").map(|m| m.trim().to_string()))
        .ok_or_else(|| Error::Other(format!("{} no declara Main-Class", jar.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_de_version_como_los_de_los_perfiles() {
        assert_eq!(version_id("1.21.1", &Loader::NeoForge { version: "21.1.251".into() }), "neoforge-21.1.251");
        assert_eq!(version_id("1.21.1", &Loader::Fabric { version: "0.16.9".into() }), "fabric-loader-0.16.9-1.21.1");
        assert_eq!(version_id("1.21.1", &Loader::Vanilla), "1.21.1");
    }

    #[test]
    fn datos_de_processor() {
        let lib = Path::new("L");
        assert_eq!(literal_or_lib("'1.21.1-2024'", lib).unwrap(), "1.21.1-2024");
        assert_eq!(
            PathBuf::from(literal_or_lib("[net.neoforged:neoforge:21.1.251:client]", lib).unwrap()),
            lib.join("net/neoforged/neoforge/21.1.251/neoforge-21.1.251-client.jar")
        );
    }
}
