先确认`XMODIFIERS=@im=fcitx`变量有没有正确设置，kde plasma桌面记得启用设置里的`fcitx5 wayland启动器`。

如果还是不行的话编辑.desktop文件修改`Exec=`后面的命令。

这些文件存放在`/usr/share/applications`和`~/.local/share/applications`。不要直接修改`/usr/share/applications`里的文件，复制一份到用户空间再改。

- 对于QT和GTK应用

  ```
  Exec= env GTK_IM_MODULE=fcitx QT_IM_MODULE=fcitx 
  ```

  `env`设置启动时的环境变量

- 对于chromium和electron应用

  以qq为例，在linuxqq 【此处】%U添加命令行参数

  ```
  --ozone-platform=wayland
  ```

  不行的话设置：

  ```
  --enable-features=UseOzonePlatform --ozone-platform=wayland --enable-wayland-ime
  ```

  示例：

  ```
  Exec=env DESKTOPINTEGRATION=false /usr/bin/linuxqq --no-sandbox --ozone-platform=wayland %U
  ```
